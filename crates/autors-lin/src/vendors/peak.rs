use async_trait::async_trait;
use autors_native::DllWrapper;

use super::{config_err, next_unique_bus_id};
use crate::device::{
    protected_id, ChecksumType, LinConfiguration, LinDevice, LinFrame, MAX_DATA_LEN,
};
use crate::error::{Error, Result};

const PLINAPI_DLL: &str = "PLinApi.dll";

/// errOK.
const PLIN_OK: u32 = 0;
const PLIN_MODE_MASTER: u8 = 2;
const PLIN_DIR_PUBLISHER: u8 = 1;
const PLIN_DIR_SUBSCRIBER_AUTO_LEN: u8 = 3;
const PLIN_CST_CLASSIC: u8 = 1;
const PLIN_CST_ENHANCED: u8 = 2;
const PLIN_CST_AUTO: u8 = 3;
const PLIN_MSG_TYPE_ERROR: u8 = 1;
const PLIN_FILTER_ALL: u64 = u64::MAX;
const PLIN_CLIENT_NAME: &[u8] = b"Master\0";
const INVALID_CLIENT: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
#[allow(dead_code)]
pub enum PeakLinHwType {
    Undefined = 0,
    UsbPro = 1,
    UsbProFd = 2,
    PLinUsb = 3,
}

fn hw_name(hardware_type: i32) -> String {
    match hardware_type {
        1 => "USB_PRO".to_owned(),
        2 => "USB_PRO_FD".to_owned(),
        3 => "PLIN_USB".to_owned(),
        other => other.to_string(),
    }
}

/// CALC_CHECKSUM_ENHANCED→cstEnhanced(2).
fn plin_checksum_type(checksum_type: ChecksumType) -> u8 {
    match checksum_type {
        ChecksumType::CalcChecksum => PLIN_CST_CLASSIC,
        ChecksumType::CalcChecksumEnhanced => PLIN_CST_ENHANCED,
    }
}

fn checksum_type_for_id(id: u8, checksum_type: ChecksumType) -> u8 {
    if matches!(id & 0x3f, 0x3c | 0x3d) {
        PLIN_CST_CLASSIC
    } else {
        plin_checksum_type(checksum_type)
    }
}

fn plin_error_name(status: u32) -> &'static str {
    match status {
        0 => "errOK",
        1 => "errTransmitterBusy",
        2 => "errWrongChecksum",
        3 => "errShortData",
        4 => "errIllegalResponse",
        5 => "errIllegalMessage",
        6 => "errInitError",
        7 => "errOverflow",
        8 => "errIllegalParameter",
        9 => "errIllegalHardware",
        10 => "errIllegalClient",
        11 => "errIllegalMessageType",
        12 => "errResourceQueue",
        13 => "errIllegalIndex",
        14 => "errIllegalFrameId",
        15 => "errLengthTooSmall",
        24 => "errNoMessage",
        25 => "errRcvQueueFull",
        26 => "errInternalError",
        _ => "err???",
    }
}

fn check(status: u32, bus_id: &str, what: &str) -> Result<()> {
    if status == PLIN_OK {
        Ok(())
    } else {
        Err(config_err(
            bus_id,
            format!(
                "PLinApi: {what} failed: {} ({status})",
                plin_error_name(status)
            ),
        ))
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct TlinMsg {
    frame_id: u8,
    len: u8,
    direction: u8,
    /// TLINChecksumType.
    checksum_type: u8,
    data: u64,
    checksum_out: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct TlinRcvMsg {
    msg_type: u8,
    frame_id: u8,
    len: u8,
    /// TLINDirection.
    #[allow(dead_code)]
    direction: u8,
    /// TLINChecksumType.
    #[allow(dead_code)]
    checksum_type: u8,
    data: [u8; 8],
    #[allow(dead_code)]
    checksum: u8,
    #[allow(dead_code)]
    error_flags: u32,
    #[allow(dead_code)]
    timestamp: u64,
    #[allow(dead_code)]
    counter: u32,
}

const _: () = assert!(std::mem::size_of::<TlinMsg>() == 13);
const _: () = assert!(std::mem::size_of::<TlinRcvMsg>() == 30);

fn build_tx_msg(id: u8, data: &[u8], checksum_type: ChecksumType) -> TlinMsg {
    let mut buf = [0u8; 8];
    buf[..data.len()].copy_from_slice(data);
    TlinMsg {
        frame_id: protected_id(id),
        len: data.len() as u8,
        direction: PLIN_DIR_PUBLISHER,
        checksum_type: checksum_type_for_id(id, checksum_type),
        data: u64::from_le_bytes(buf),
        checksum_out: 0,
    }
}

fn build_request_msg(id: u8) -> TlinMsg {
    TlinMsg {
        frame_id: protected_id(id),
        len: MAX_DATA_LEN as u8,
        direction: PLIN_DIR_SUBSCRIBER_AUTO_LEN,
        checksum_type: if matches!(id & 0x3f, 0x3c | 0x3d) {
            PLIN_CST_CLASSIC
        } else {
            PLIN_CST_AUTO
        },
        data: 0,
        checksum_out: 0,
    }
}

#[derive(Debug)]
struct PLinApi {
    _dll: DllWrapper,
    /// TLINError LIN_ConnectClient(HLINCLIENT hClient, HLINHW hHw).
    lin_connect_client: unsafe extern "system" fn(u8, u16) -> u32,
    /// TLINError LIN_DisconnectClient(HLINCLIENT hClient, HLINHW hHw).
    lin_disconnect_client: unsafe extern "system" fn(u8, u16) -> u32,
    /// TLINError LIN_ResetClient(HLINCLIENT hClient).
    #[allow(dead_code)]
    lin_reset_client: unsafe extern "system" fn(u8) -> u32,
    /// TLINError LIN_RemoveClient(HLINCLIENT hClient).
    lin_remove_client: unsafe extern "system" fn(u8) -> u32,
    /// TLINError LIN_RegisterClient(LPCSTR strName, DWORD hWnd,
    /// HLINCLIENT *phClient).
    lin_register_client: unsafe extern "system" fn(*const u8, u32, *mut u8) -> u32,
    /// TLINError LIN_SetClientFilter(HLINCLIENT, HLINHW, unsigned __int64
    /// hRcvFilter).
    lin_set_client_filter: unsafe extern "system" fn(u8, u16, u64) -> u32,
    /// TLINError LIN_SetClientParam(HLINCLIENT, TLINClientParam wParam,
    /// DWORD dwValue).
    #[allow(dead_code)]
    lin_set_client_param: unsafe extern "system" fn(u8, u16, u32) -> u32,
    /// TLINError LIN_Read(HLINCLIENT hClient, TLINRcvMsg *pRcvMsg).
    lin_read: unsafe extern "system" fn(u8, *mut TlinRcvMsg) -> u32,
    /// TLINError LIN_Write(HLINCLIENT hClient, HLINHW hHw, TLINMsg *pSndMsg).
    lin_write: unsafe extern "system" fn(u8, u16, *mut TlinMsg) -> u32,
    /// TLINError LIN_InitializeHardware(HLINCLIENT, HLINHW, TLINMode,
    /// WORD wBaudrate).
    lin_initialize_hardware: unsafe extern "system" fn(u8, u16, u8, u16) -> u32,
    /// TLINError LIN_CalculateChecksum(TLINMsg *pMsg).
    lin_calculate_checksum: unsafe extern "system" fn(*mut TlinMsg) -> u32,
}

impl PLinApi {
    fn load() -> Result<Self> {
        Self::load_from(PLINAPI_DLL)
    }

    fn load_from(path: &str) -> Result<Self> {
        let dll = DllWrapper::load(path)
            .map_err(|e| Error::Driver(format!("failed to load {path}: {e}")))?;
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let s: libloading::Symbol<$ty> = unsafe { dll.library().get::<$ty>($name) }
                    .map_err(|e| {
                        Error::Driver(format!(
                            "{}: missing export {:?}: {}",
                            path,
                            String::from_utf8_lossy($name),
                            e
                        ))
                    })?;
                *s
            }};
        }
        Ok(Self {
            lin_connect_client: sym!(
                b"LIN_ConnectClient\0",
                unsafe extern "system" fn(u8, u16) -> u32
            ),
            lin_disconnect_client: sym!(
                b"LIN_DisconnectClient\0",
                unsafe extern "system" fn(u8, u16) -> u32
            ),
            lin_reset_client: sym!(b"LIN_ResetClient\0", unsafe extern "system" fn(u8) -> u32),
            lin_remove_client: sym!(b"LIN_RemoveClient\0", unsafe extern "system" fn(u8) -> u32),
            lin_register_client: sym!(
                b"LIN_RegisterClient\0",
                unsafe extern "system" fn(*const u8, u32, *mut u8) -> u32
            ),
            lin_set_client_filter: sym!(
                b"LIN_SetClientFilter\0",
                unsafe extern "system" fn(u8, u16, u64) -> u32
            ),
            lin_set_client_param: sym!(
                b"LIN_SetClientParam\0",
                unsafe extern "system" fn(u8, u16, u32) -> u32
            ),
            lin_read: sym!(
                b"LIN_Read\0",
                unsafe extern "system" fn(u8, *mut TlinRcvMsg) -> u32
            ),
            lin_write: sym!(
                b"LIN_Write\0",
                unsafe extern "system" fn(u8, u16, *mut TlinMsg) -> u32
            ),
            lin_initialize_hardware: sym!(
                b"LIN_InitializeHardware\0",
                unsafe extern "system" fn(u8, u16, u8, u16) -> u32
            ),
            lin_calculate_checksum: sym!(
                b"LIN_CalculateChecksum\0",
                unsafe extern "system" fn(*mut TlinMsg) -> u32
            ),
            _dll: dll,
        })
    }
}

pub struct PeakLin {
    api: PLinApi,
    unique_bus_id: i32,
    client: u8,
    hw: u16,
    dlc: u8,
    checksum_type: ChecksumType,
    bus_id: String,
}

impl PeakLin {
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: PLinApi::load()?,
            unique_bus_id: next_unique_bus_id(),
            client: INVALID_CLIENT,
            hw: PeakLinHwType::Undefined as u16,
            dlc: 0,
            checksum_type: ChecksumType::default(),
            bus_id: String::new(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.client != INVALID_CLIENT
    }

    fn close_sync(&mut self) {
        if self.client == INVALID_CLIENT {
            return;
        }
        if self.hw > 0 {
            unsafe {
                (self.api.lin_disconnect_client)(self.client, self.hw);
            }
            self.hw = PeakLinHwType::Undefined as u16;
        }
        unsafe {
            (self.api.lin_remove_client)(self.client);
        }
        self.client = INVALID_CLIENT;
    }
}

#[async_trait]
impl LinDevice for PeakLin {
    fn unique_bus_id(&self) -> i32 {
        self.unique_bus_id
    }

    fn is_available(&self) -> bool {
        self.is_open()
    }

    async fn open(&mut self, config: &LinConfiguration) -> Result<bool> {
        if self.client != INVALID_CLIENT {
            return Ok(true);
        }
        self.bus_id = config.bus_id.clone().unwrap_or_else(|| {
            format!(
                "Peak/{}/LIN{}",
                hw_name(config.hardware_type),
                config.channel + 1
            )
        });
        self.dlc = config.dlc;
        self.checksum_type = config.checksum_type;
        let mut client: u8 = 0;
        let st =
            unsafe { (self.api.lin_register_client)(PLIN_CLIENT_NAME.as_ptr(), 0, &mut client) };
        if st != PLIN_OK {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "PLinApi: LIN_RegisterClient failed: {} ({st})",
                    plin_error_name(st)
                ),
            ));
        }
        self.client = client;
        let hw = config.hardware_type as u16;
        let st = unsafe { (self.api.lin_connect_client)(client, hw) };
        if st != PLIN_OK {
            return Err(config_err(
                &self.bus_id,
                format!(
                    "PLinApi: LIN_ConnectClient(hw={}) failed: {} ({st})",
                    hw_name(config.hardware_type),
                    plin_error_name(st)
                ),
            ));
        }
        self.hw = hw;
        let result = (|| {
            let st = unsafe {
                (self.api.lin_initialize_hardware)(client, hw, PLIN_MODE_MASTER, config.baudrate)
            };
            check(st, &self.bus_id, "LIN_InitializeHardware")?;
            let st = unsafe { (self.api.lin_set_client_filter)(client, hw, PLIN_FILTER_ALL) };
            check(st, &self.bus_id, "LIN_SetClientFilter")
        })();
        if let Err(e) = result {
            self.close_sync();
            return Err(e);
        }
        Ok(true)
    }

    async fn close(&mut self) {
        self.close_sync();
    }

    async fn send(&mut self, id: u8, data: &[u8]) -> Result<usize> {
        if self.hw == 0 || self.client == INVALID_CLIENT {
            return Ok(0);
        }
        if data.len() > usize::from(self.dlc) || data.len() > MAX_DATA_LEN {
            return Err(Error::Invalid(format!(
                "payload length {} exceeds LIN DLC {} (max {MAX_DATA_LEN})",
                data.len(),
                self.dlc
            )));
        }
        let mut msg = build_tx_msg(id, data, self.checksum_type);
        unsafe {
            (self.api.lin_calculate_checksum)(&mut msg);
        }
        let st = unsafe { (self.api.lin_write)(self.client, self.hw, &mut msg) };
        if st != PLIN_OK {
            return Ok(0);
        }
        Ok(data.len())
    }

    async fn request(&mut self, id: u8) -> Result<bool> {
        if self.hw == 0 || self.client == INVALID_CLIENT {
            return Ok(false);
        }
        let mut msg = build_request_msg(id);
        let status = unsafe { (self.api.lin_write)(self.client, self.hw, &mut msg) };
        Ok(status == PLIN_OK)
    }

    async fn on_receive(&mut self) -> Result<Option<LinFrame>> {
        if self.client == INVALID_CLIENT {
            return Ok(None);
        }
        let mut msg = TlinRcvMsg::default();
        let st = unsafe { (self.api.lin_read)(self.client, &mut msg) };
        if st != PLIN_OK || msg.msg_type == PLIN_MSG_TYPE_ERROR {
            return Ok(None);
        }
        let (frame_id, len, direction) = (msg.frame_id & 0x3f, msg.len, msg.direction);
        if len > MAX_DATA_LEN as u8 {
            return Ok(None);
        }
        let data = msg.data;
        let frame = LinFrame::with_len(
            &self.bus_id,
            frame_id,
            data.to_vec(),
            len,
            direction == PLIN_DIR_PUBLISHER,
        )?;
        Ok(Some(frame))
    }
}

impl Drop for PeakLin {
    fn drop(&mut self) {
        self.close_sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_have_expected_layout() {
        assert_eq!(std::mem::size_of::<TlinMsg>(), 13);
        assert_eq!(std::mem::size_of::<TlinRcvMsg>(), 30);
    }

    #[test]
    fn hw_type_values_and_names() {
        assert_eq!(PeakLinHwType::Undefined as u16, 0);
        assert_eq!(PeakLinHwType::UsbPro as u16, 1);
        assert_eq!(PeakLinHwType::UsbProFd as u16, 2);
        assert_eq!(PeakLinHwType::PLinUsb as u16, 3);
        assert_eq!(hw_name(1), "USB_PRO");
        assert_eq!(hw_name(2), "USB_PRO_FD");
        assert_eq!(hw_name(3), "PLIN_USB");
        assert_eq!(hw_name(0), "0");
        assert_eq!(hw_name(42), "42");
    }

    #[test]
    fn checksum_type_mapping() {
        assert_eq!(
            plin_checksum_type(ChecksumType::CalcChecksum),
            PLIN_CST_CLASSIC
        );
        assert_eq!(
            plin_checksum_type(ChecksumType::CalcChecksumEnhanced),
            PLIN_CST_ENHANCED
        );
    }

    #[test]
    fn error_names() {
        assert_eq!(plin_error_name(0), "errOK");
        assert_eq!(plin_error_name(9), "errIllegalHardware");
        assert_eq!(plin_error_name(24), "errNoMessage");
        assert_eq!(plin_error_name(0x123), "err???");
    }

    #[test]
    fn check_maps_error() {
        assert!(check(0, "BUS", "x").is_ok());
        match check(9, "Peak/USB_PRO/LIN1", "LIN_ConnectClient").unwrap_err() {
            Error::Driver(msg) => {
                assert!(msg.contains("Peak/USB_PRO/LIN1"));
                assert!(msg.contains("LIN_ConnectClient"));
                assert!(msg.contains("errIllegalHardware"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn build_tx_msg_fields() {
        let msg = build_tx_msg(0x3C, &[0x02, 0x10], ChecksumType::CalcChecksum);
        let (id, len, dir, cst, data, out) = (
            msg.frame_id,
            msg.len,
            msg.direction,
            msg.checksum_type,
            msg.data,
            msg.checksum_out,
        );
        assert_eq!(id, 0x3C);
        assert_eq!(len, 2);
        assert_eq!(dir, PLIN_DIR_PUBLISHER);
        assert_eq!(cst, PLIN_CST_CLASSIC);
        assert_eq!(data.to_le_bytes()[..2], [0x02, 0x10]);
        assert_eq!(out, 0);
        let msg = build_tx_msg(0x3D, &[0xFF; 8], ChecksumType::CalcChecksumEnhanced);
        assert_eq!(msg.frame_id, 0x7D);
        assert_eq!(msg.checksum_type, PLIN_CST_CLASSIC);
        assert_eq!(msg.len, 8);
        let request = build_request_msg(0x3D);
        assert_eq!(request.frame_id, 0x7D);
        assert_eq!(request.direction, PLIN_DIR_SUBSCRIBER_AUTO_LEN);
        assert_eq!(request.checksum_type, PLIN_CST_CLASSIC);
    }

    #[test]
    fn missing_dll_is_driver_error() {
        let err = PLinApi::load_from("no_such_plinapi_xyz.dll").unwrap_err();
        assert!(matches!(err, Error::Driver(_)), "got: {err:?}");
    }

    #[cfg(feature = "blocking")]
    #[test]
    fn load_real_driver_or_driver_error() {
        match PeakLin::new() {
            Ok(mut dev) => {
                assert!(!dev.is_open());
                assert!(!dev.is_available());
                assert!(dev.unique_bus_id() >= 1);
                assert_eq!(autors_runtime::block_on(dev.send(0x3C, &[1])).unwrap(), 0);
                assert!(autors_runtime::block_on(dev.on_receive())
                    .unwrap()
                    .is_none());
                autors_runtime::block_on(dev.close());
            }
            Err(Error::Driver(_)) => {}
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }
}
