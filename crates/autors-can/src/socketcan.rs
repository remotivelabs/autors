//! Linux SocketCAN backend (`#[cfg(target_os = "linux")]`).
//! Implements [`CanDevice`] on Linux on top of the kernel's SocketCAN subsystem:
//! - `open` configures the bitrate/FD mode and brings the interface up via netlink
//!   (`socketcan::nl::CanInterface`); this requires CAP_NET_ADMIN. When no bitrate
//!   is set, netlink is skipped and the interface is assumed to be already configured;
//! - transmit/receive go through `CanSocket` (classic) or `CanFdSocket` (FD), non-blocking;
//! - SocketCAN error frames are surfaced via [`CanDevice::poll_error`]
//!   (vendor driver adapters discard error frames);
//! - remote frames have no counterpart in the shared frame model and are dropped
//!   on receive (the same filtering applied by the Kvaser driver adapter).
//! No hardware is available for on-target verification here; only compilation and
//! construction-level/pure-function tests are covered.

use std::collections::VecDeque;

use async_trait::async_trait;
use socketcan::frame::FdFlags;
use socketcan::{
    CanAnyFrame, CanCtrlMode, CanDataFrame, CanFdFrame, CanFdSocket, CanFrame as ScCanFrame,
    CanInterface, CanSocket, EmbeddedFrame, ExtendedId, Frame as ScFrame, Id, ShouldRetry, Socket,
    SocketOptions, StandardId,
};

use crate::device::{config_err, format_bus_id, CanDevice, DeviceCore, CAN_EXT_FLAG};
use crate::error::{Error, Result};
use crate::frame::{CanBaudrate, CanConfiguration, CanFdBaudrate, CanFrame, FrameType};

/// SocketCAN backend device.
pub struct SocketCanDevice {
    core: DeviceCore,
    /// Explicit interface name (e.g. `vcan0`); when `None`, the name is derived
    /// from the configured channel number as `can{channel}`.
    iface: Option<String>,
    bus_id: Option<String>,
    config: Option<CanConfiguration>,
    socket: Option<SocketKind>,
    /// Bus error frame descriptions buffered on the receive path (consumed by `poll_error`).
    pending_errors: VecDeque<String>,
}

enum SocketKind {
    Classic(CanSocket),
    Fd(CanFdSocket),
}

/// Result of a single kernel read in `receive` (keeps the borrow in the smallest scope).
enum Rx {
    Frame(CanFrame),
    /// Remote frame: dropped (the shared `CanFrame` model has no remote-frame concept).
    Remote,
    /// SocketCAN error frame description.
    Error(String),
    /// Non-blocking read returned no data.
    Empty,
}

impl SocketCanDevice {
    /// Constructs a device that derives the interface name from the channel number (`can{channel}`).
    pub fn new() -> Self {
        Self {
            core: DeviceCore::new(),
            iface: None,
            bus_id: None,
            config: None,
            socket: None,
            pending_errors: VecDeque::new(),
        }
    }

    /// Constructs a device with an explicit interface name (e.g. `vcan0`, `slcan0`),
    /// ignoring the channel number in the configuration.
    pub fn with_iface(iface: impl Into<String>) -> Self {
        Self {
            iface: Some(iface.into()),
            ..Self::new()
        }
    }

    fn iface_name(&self, config: &CanConfiguration) -> String {
        self.iface
            .clone()
            .unwrap_or_else(|| format!("can{}", config.channel))
    }

    /// netlink configuration: set the bitrate/FD mode and bring the interface up (requires CAP_NET_ADMIN).
    fn configure_interface(iface: &str, config: &CanConfiguration) -> Result<()> {
        let nl = CanInterface::open(iface)
            .map_err(|e| config_err(iface, format!("open interface failed: {e:?}")))?;
        nl.bring_down()
            .map_err(|e| config_err(iface, format!("bring down failed: {e:?}")))?;
        nl.set_bitrate(config.baudrate.as_u32(), None::<u32>)
            .map_err(|e| {
                config_err(
                    iface,
                    format!("set bitrate {} failed: {e:?}", config.bit_rate_str()),
                )
            })?;
        if config.is_fd() {
            nl.set_ctrlmode(CanCtrlMode::Fd, true)
                .map_err(|e| config_err(iface, format!("enable FD mode failed: {e:?}")))?;
            if let Some(cfg) = &config.fd_bit_rate_config {
                if cfg.non_iso {
                    nl.set_ctrlmode(CanCtrlMode::NonIso, true).map_err(|e| {
                        config_err(iface, format!("enable FD non-ISO mode failed: {e:?}"))
                    })?;
                }
                // Full bit timing: netlink requires exact tseg/brp/sjw values; here only the
                // data-phase bitrate is set (full BitRatePar-style timing parameters are not applied).
            }
            if config.baudrate_fd != CanFdBaudrate::NotUsed {
                nl.set_data_bitrate(config.baudrate_fd.as_u32(), None::<u32>)
                    .map_err(|e| config_err(iface, format!("set data bitrate failed: {e:?}")))?;
            }
        }
        nl.bring_up()
            .map_err(|e| config_err(iface, format!("bring up failed: {e:?}")))?;
        Ok(())
    }

    /// Assembles a SocketCAN `Id` from the raw ID plus the extended-frame flag.
    fn build_id(can_id: u32) -> Result<Id> {
        let raw = can_id & 0x7FFF_FFFF;
        if can_id & CAN_EXT_FLAG != 0 {
            ExtendedId::new(raw)
                .map(Id::Extended)
                .ok_or_else(|| Error::Invalid(format!("invalid extended CAN ID {raw:#X}")))
        } else {
            StandardId::new(raw as u16)
                .map(Id::Standard)
                .ok_or_else(|| Error::Invalid(format!("invalid standard CAN ID {raw:#X}")))
        }
    }

    fn classic_frame(bus_id: &str, f: &CanDataFrame) -> CanFrame {
        Self::convert_frame(
            bus_id,
            f.raw_id(),
            f.is_extended(),
            f.data(),
            FrameType::CAN20B,
        )
    }

    fn fd_frame(bus_id: &str, f: &CanFdFrame) -> CanFrame {
        let frame_type = if f.is_brs() {
            FrameType::FD_BRS
        } else {
            FrameType::FD
        };
        Self::convert_frame(bus_id, f.raw_id(), f.is_extended(), f.data(), frame_type)
    }

    fn convert_frame(
        bus_id: &str,
        raw_id: u32,
        is_extended: bool,
        data: &[u8],
        frame_type: FrameType,
    ) -> CanFrame {
        let id = if is_extended {
            raw_id | CAN_EXT_FLAG
        } else {
            raw_id
        };
        CanFrame::new(bus_id, id, data.to_vec(), false, frame_type)
    }

    /// Performs a single (non-blocking) read from the socket.
    fn read_once(socket: &SocketKind, bus_id: &str) -> Result<Rx> {
        match socket {
            SocketKind::Classic(s) => match s.read_frame() {
                Ok(ScCanFrame::Data(df)) => Ok(Rx::Frame(Self::classic_frame(bus_id, &df))),
                Ok(ScCanFrame::Remote(_)) => Ok(Rx::Remote),
                Ok(ScCanFrame::Error(ef)) => Ok(Rx::Error(ef.into_error().to_string())),
                Err(e) if e.should_retry() => Ok(Rx::Empty),
                Err(e) => Err(Error::Io(e)),
            },
            SocketKind::Fd(s) => match s.read_frame() {
                Ok(CanAnyFrame::Normal(df)) => Ok(Rx::Frame(Self::classic_frame(bus_id, &df))),
                Ok(CanAnyFrame::Fd(fdf)) => Ok(Rx::Frame(Self::fd_frame(bus_id, &fdf))),
                Ok(CanAnyFrame::Remote(_)) => Ok(Rx::Remote),
                Ok(CanAnyFrame::Error(ef)) => Ok(Rx::Error(ef.into_error().to_string())),
                Err(e) if e.should_retry() => Ok(Rx::Empty),
                Err(e) => Err(Error::Io(e)),
            },
        }
    }
}

impl Default for SocketCanDevice {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CanDevice for SocketCanDevice {
    fn core(&self) -> &DeviceCore {
        &self.core
    }

    fn core_mut(&mut self) -> &mut DeviceCore {
        &mut self.core
    }

    /// Available once opened; otherwise probes the interface by opening it to verify it exists.
    async fn is_available(&mut self) -> Result<bool> {
        if self.socket.is_some() {
            return Ok(true);
        }
        match &self.iface {
            Some(name) => Ok(CanSocket::open(name).is_ok()),
            None => Ok(false),
        }
    }

    /// Opens and configures the interface.
    /// The generated `BusId` uses `socketcan` as the device name; a `bus_id`
    /// already present in the configuration is kept as-is.
    async fn open(&mut self, config: CanConfiguration) -> Result<bool> {
        let iface = self.iface_name(&config);
        // Only touch netlink when a bitrate is explicitly set (requires CAP_NET_ADMIN);
        // otherwise assume the interface is already configured by the system
        // (ip link set can0 up type can bitrate ...).
        if config.baudrate != CanBaudrate::NotSet || config.fd_bit_rate_config.is_some() {
            Self::configure_interface(&iface, &config)?;
        }
        let socket = if config.is_fd() {
            SocketKind::Fd(CanFdSocket::open(&iface)?)
        } else {
            SocketKind::Classic(CanSocket::open(&iface)?)
        };
        match &socket {
            SocketKind::Classic(s) => {
                s.set_nonblocking(true)?;
                s.set_error_filter_accept_all()?;
            }
            SocketKind::Fd(s) => {
                s.set_nonblocking(true)?;
                s.set_error_filter_accept_all()?;
            }
        }
        self.bus_id = Some(
            config
                .bus_id
                .clone()
                .unwrap_or_else(|| format_bus_id("socketcan", config.channel)),
        );
        self.config = Some(config);
        self.socket = Some(socket);
        Ok(true)
    }

    /// Closes the device; the socket is closed on drop.
    async fn close(&mut self) {
        self.socket = None;
        self.config = None;
        self.pending_errors.clear();
    }

    /// Sends a frame; returns `Ok(0)` when the device is not opened.
    async fn send(&mut self, can_id: u32, data: &[u8], frame_type: FrameType) -> Result<usize> {
        let socket = match &self.socket {
            Some(socket) => socket,
            None => return Ok(0),
        };
        let id = Self::build_id(can_id)?;
        match socket {
            SocketKind::Classic(s) => {
                if !frame_type.is_classic() {
                    return Err(Error::NotSupported(
                        "CAN FD frame requested on a classic CAN socket".to_string(),
                    ));
                }
                let frame = ScCanFrame::new(id, data).ok_or_else(|| {
                    Error::Invalid(format!(
                        "{} bytes too long for a classic CAN frame",
                        data.len()
                    ))
                })?;
                s.write_frame(&frame)?;
            }
            SocketKind::Fd(s) => {
                if frame_type.is_classic() {
                    let frame = CanDataFrame::new(id, data).ok_or_else(|| {
                        Error::Invalid(format!(
                            "{} bytes too long for a classic CAN frame",
                            data.len()
                        ))
                    })?;
                    s.write_frame(&frame)?;
                } else {
                    let flags = if frame_type.contains(FrameType::BRS) {
                        FdFlags::BRS
                    } else {
                        FdFlags::empty()
                    };
                    let frame = CanFdFrame::with_flags(id, data, flags).ok_or_else(|| {
                        Error::Invalid(format!("{} bytes too long for a CAN FD frame", data.len()))
                    })?;
                    s.write_frame(&frame)?;
                }
            }
        }
        // Record send statistics and return the data length.
        let bus_id = self.bus_id.clone().unwrap_or_default();
        let frame = CanFrame::new(bus_id, can_id, data.to_vec(), true, frame_type);
        Ok(self.core.record_sent(&frame))
    }

    /// Non-blocking receive; remote frames are dropped, error frames are buffered for `poll_error`.
    async fn receive(&mut self) -> Result<Option<CanFrame>> {
        let bus_id = self.bus_id.clone().unwrap_or_default();
        loop {
            let socket = match &self.socket {
                Some(socket) => socket,
                None => return Ok(None),
            };
            match Self::read_once(socket, &bus_id)? {
                Rx::Frame(frame) => return Ok(Some(frame)),
                Rx::Empty => return Ok(None),
                Rx::Remote => continue,
                Rx::Error(desc) => {
                    self.pending_errors.push_back(desc);
                }
            }
        }
    }

    /// SocketCAN-specific extension: takes a buffered bus error frame description.
    async fn poll_error(&mut self) -> Result<Option<String>> {
        Ok(self.pending_errors.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::ALL_FRAMES;

    /// Compile-time object-safety assertion.
    fn _assert_object_safe(_: &dyn CanDevice) {}

    #[test]
    fn construction_and_object_safety() {
        let p = SocketCanDevice::new();
        _assert_object_safe(&p);
        assert!(p.iface.is_none());
        let p = SocketCanDevice::with_iface("vcan0");
        assert_eq!(p.iface.as_deref(), Some("vcan0"));
        let boxed: Box<dyn CanDevice> = Box::new(p);
        assert!(boxed.unique_bus_id() >= 1);
    }

    #[test]
    fn iface_name_derivation() {
        let p = SocketCanDevice::new();
        let cfg = CanConfiguration::new(3, CanBaudrate::B500Kbit, CanFdBaudrate::NotUsed);
        assert_eq!(p.iface_name(&cfg), "can3");
        let p = SocketCanDevice::with_iface("vcan0");
        assert_eq!(p.iface_name(&cfg), "vcan0");
    }

    #[test]
    fn send_receive_when_not_open() {
        let mut p = SocketCanDevice::new();
        // When not opened, send returns 0 and receive yields no frame.
        assert_eq!(
            autors_runtime::block_on(p.send(0x123, &[1, 2, 3], FrameType::CAN20B)).unwrap(),
            0
        );
        assert!(autors_runtime::block_on(p.receive()).unwrap().is_none());
        assert!(autors_runtime::block_on(p.poll_error()).unwrap().is_none());
    }

    #[test]
    fn is_available_without_iface() {
        let mut p = SocketCanDevice::new();
        assert!(!autors_runtime::block_on(p.is_available()).unwrap());
        // Nonexistent interface -> false (no panic)
        let mut p = SocketCanDevice::with_iface("definitely_no_such_can_iface_9");
        assert!(!autors_runtime::block_on(p.is_available()).unwrap());
    }

    #[test]
    fn build_id_flag_mapping() {
        match SocketCanDevice::build_id(0x123).unwrap() {
            Id::Standard(id) => assert_eq!(id.as_raw(), 0x123),
            Id::Extended(_) => panic!("expected standard id"),
        }
        match SocketCanDevice::build_id(0x8000_0123).unwrap() {
            Id::Extended(id) => assert_eq!(id.as_raw(), 0x123),
            Id::Standard(_) => panic!("expected extended id"),
        }
        assert!(SocketCanDevice::build_id(0x800).is_err()); // standard frame above 0x7FF
        assert!(SocketCanDevice::build_id(0xA000_0000).is_err()); // extended frame above 29 bits
    }

    #[test]
    fn frame_conversion_from_socketcan_types() {
        // Classic data frame (socketcan frame construction is a pure data operation, no kernel needed)
        let df =
            CanDataFrame::new(Id::Extended(ExtendedId::new(0x123).unwrap()), &[1, 2, 3]).unwrap();
        let f = SocketCanDevice::classic_frame("bus", &df);
        assert_eq!(f.id, 0x8000_0123);
        assert!(f.is_extended_id());
        assert_eq!(f.data, vec![1, 2, 3]);
        assert_eq!(f.frame_type, FrameType::CAN20B);
        assert!(!f.is_master_frame);

        let df = CanDataFrame::new(Id::Standard(StandardId::new(0x7FF).unwrap()), &[]).unwrap();
        let f = SocketCanDevice::classic_frame("bus", &df);
        assert_eq!(f.id, 0x7FF);
        assert!(!f.is_extended_id());

        // FD frame (constructed with the BRS flag)
        let fdf = CanFdFrame::with_flags(
            Id::Standard(StandardId::new(0x123).unwrap()),
            &[0u8; 12],
            FdFlags::BRS,
        )
        .unwrap();
        let f = SocketCanDevice::fd_frame("bus", &fdf);
        assert_eq!(f.frame_type, FrameType::FD_BRS);
        let fdf = CanFdFrame::with_flags(
            Id::Standard(StandardId::new(0x123).unwrap()),
            &[0u8; 12],
            FdFlags::empty(),
        )
        .unwrap();
        let f = SocketCanDevice::fd_frame("bus", &fdf);
        assert_eq!(f.frame_type, FrameType::FD);
        assert_eq!(f.data.len(), 12);
    }

    #[test]
    fn bus_id_fallback_format() {
        // Fallback bus_id format when none is given explicitly
        assert_eq!(format_bus_id("socketcan", 0), "socketcan/CAN1");
        assert_eq!(ALL_FRAMES, u32::MAX);
    }
}
