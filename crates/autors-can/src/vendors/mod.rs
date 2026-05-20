//! CAN hardware vendor adapters (Windows native drivers, dynamically loaded).
//! One adapter module per vendor hardware family:
//! - `kvaser`: Kvaser interfaces, loads `canlib32.dll` (feature `vendor-kvaser`);
//! - `vector`: Vector interfaces, loads `vxlapi64.dll`/`vxlapi.dll`
//!   (feature `vendor-vector`);
//! - `peak`: PEAK-System interfaces, loads `PCANBasic.dll` (feature `vendor-peak`);
//! - `eight_devices`: 8devices USB2CAN, loads `usb2can.dll` (CANAL API)
//!   (feature `vendor-8devices`);
//! - `advantech`: Advantech interfaces, drives the Advantech kernel driver
//!   directly via kernel32 `DeviceIoControl` (no user-mode vendor DLL)
//!   (feature `vendor-advantech`);
//! - `can_analyst`: CANalyst-II interfaces, loads `ControlCAN.dll`
//!   (feature `vendor-can-analyst`);
//! - `esd`: ESD interfaces via NTCAN, loads `ntcan64.dll`/`ntcan.dll`
//!   (feature `vendor-esd`);
//! - `etas`: ETAS interfaces via BOA OCI (two DLLs) (feature `vendor-etas`);
//! - `eb_el`: Eberspächer FlexCard, loads `fcbase.dll` (feature `vendor-eb-el`);
//! - `ime_actia`: I+ME Actia interfaces, locates the LevelX DLL through the
//!   registry (feature `vendor-ime-actia`);
//! - `intrepid`: Intrepid Control Systems neoVI, loads `icsneo40.dll`
//!   (feature `vendor-intrepid`);
//! - `ixxat`: IXXAT VCI, loads `vcinpl.dll`/`vcinpl2.dll` (feature `vendor-ixxat`);
//! - `lawicel`: Lawicel CANUSB, loads `canusbdrv64.dll`/`canusbdrv.dll`
//!   (feature `vendor-lawicel`);
//! - `mhs`: MHS Tiny-CAN, loads `mhstcan.dll` (feature `vendor-mhs`);
//! - `ni`: National Instruments NI-CAN, loads `nican.dll` (feature `vendor-ni`);
//! - `tosun`: Tosun TSMaster, loads `libTSCAN.dll` (feature `vendor-tosun`);
//! - `elm327`: ELM327 AT-command serial protocol (not a DLL vendor,
//!   cross-platform); real serial-port wiring lives in `elm327_serial`
//!   (both gated by feature `vendor-elm327`).
//!
//! The sixteen DLL/driver adapters are all `#[cfg(windows)]` and use libloading
//! to dlopen at runtime (advantech calls kernel32 directly). If the driver is
//! not installed (or an exported symbol is missing), construction returns
//! [`crate::Error::Driver`] instead of panicking. `elm327` is a pure-Rust
//! protocol implementation with no platform restriction.
//! Enablement: each vendor is gated by its own cargo feature (see above; all
//! off by default), and `all-vendors` enables all 17 at once. On non-Windows
//! platforms the vendor features exist but compile no code (the modules remain
//! excluded by `cfg(windows)`).
//! Shared frame/configuration types live in [`crate::frame`]; the unified
//! [`crate::device::CanDevice`] trait, [`crate::device::DeviceCore`],
//! and helpers such as ID masks and DLC conversion live in
//! [`crate::device`] (the Linux backend lives in `crate::socketcan`, with
//! stubs on other platforms).

#[cfg(windows)]
#[cfg(feature = "vendor-advantech")]
pub mod advantech;
#[cfg(windows)]
#[cfg(feature = "vendor-can-analyst")]
pub mod can_analyst;
#[cfg(windows)]
#[cfg(feature = "vendor-eb-el")]
pub mod eb_el;
#[cfg(windows)]
#[cfg(feature = "vendor-8devices")]
pub mod eight_devices;
#[cfg(feature = "vendor-elm327")]
pub mod elm327;
#[cfg(feature = "vendor-elm327")]
pub mod elm327_serial;
#[cfg(windows)]
#[cfg(feature = "vendor-esd")]
pub mod esd;
#[cfg(windows)]
#[cfg(feature = "vendor-etas")]
pub mod etas;
#[cfg(windows)]
#[cfg(feature = "vendor-ime-actia")]
pub mod ime_actia;
#[cfg(windows)]
#[cfg(feature = "vendor-intrepid")]
pub mod intrepid;
#[cfg(windows)]
#[cfg(feature = "vendor-ixxat")]
pub mod ixxat;
#[cfg(windows)]
#[cfg(feature = "vendor-kvaser")]
pub mod kvaser;
#[cfg(windows)]
#[cfg(feature = "vendor-lawicel")]
pub mod lawicel;
#[cfg(windows)]
#[cfg(feature = "vendor-mhs")]
pub mod mhs;
#[cfg(windows)]
#[cfg(feature = "vendor-ni")]
pub mod ni;
#[cfg(windows)]
#[cfg(feature = "vendor-peak")]
pub mod peak;
#[cfg(windows)]
#[cfg(feature = "vendor-tosun")]
pub mod tosun;
#[cfg(windows)]
#[cfg(feature = "vendor-vector")]
pub mod vector;
