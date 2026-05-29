//! LIN hardware vendor adapters (dynamic loading of native Windows drivers).
//! - `kvaser`: Kvaser adapter, loads Kvaser `linlib.dll` (feature `vendor-kvaser`);
//! - `vector`: Vector adapter, loads Vector `vxlapi64.dll`/`vxlapi.dll`
//!   (feature `vendor-vector`);
//! - `peak`: PEAK adapter, loads PEAK `PLinApi.dll` (feature `vendor-peak`).
//!
//! All three adapters are `#[cfg(windows)]` and use libloading to dlopen the
//! driver at runtime; when the driver is not installed (or an exported symbol is
//! missing), construction returns [`crate::Error::Driver`] instead of panicking.
//! Each vendor is gated by its own cargo feature (all off by default), and
//! `all-vendors` enables all three at once; on non-Windows platforms the vendor
//! features exist but compile no code. The unified frame/configuration types and
//! the [`crate::device::LinDevice`] trait live in [`crate::device`].

/// Only compiled when any vendor feature is enabled (otherwise dead code on Windows).
#[cfg(all(
    windows,
    any(
        feature = "vendor-kvaser",
        feature = "vendor-peak",
        feature = "vendor-vector"
    )
))]
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(all(
    windows,
    any(
        feature = "vendor-kvaser",
        feature = "vendor-peak",
        feature = "vendor-vector"
    )
))]
use crate::error::Error;

#[cfg(windows)]
#[cfg(feature = "vendor-kvaser")]
pub mod kvaser;
#[cfg(windows)]
#[cfg(feature = "vendor-peak")]
pub mod peak;
#[cfg(windows)]
#[cfg(feature = "vendor-vector")]
pub mod vector;

/// Process-wide unique bus ID counter (statically incremented; the first ID is 1).
#[cfg(all(
    windows,
    any(
        feature = "vendor-kvaser",
        feature = "vendor-peak",
        feature = "vendor-vector"
    )
))]
static UNIQUE_BUS_ID: AtomicI32 = AtomicI32::new(0);

/// Allocate a unique bus ID for a device instance.
#[cfg(all(
    windows,
    any(
        feature = "vendor-kvaser",
        feature = "vendor-peak",
        feature = "vendor-vector"
    )
))]
pub(crate) fn next_unique_bus_id() -> i32 {
    UNIQUE_BUS_ID.fetch_add(1, Ordering::SeqCst) + 1
}

/// Build a configuration error (same convention as autors-can's `config_err`):
/// the hardware ID is merged into the [`Error::Driver`] message.
#[cfg(all(
    windows,
    any(
        feature = "vendor-kvaser",
        feature = "vendor-peak",
        feature = "vendor-vector"
    )
))]
pub(crate) fn config_err(hardware_id: &str, message: impl std::fmt::Display) -> Error {
    Error::Driver(format!("{hardware_id}: {message}"))
}
