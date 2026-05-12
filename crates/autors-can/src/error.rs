//! Error types.

/// Unified error type for the CAN layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Driver/adapter-layer error.
    #[error("driver error: {0}")]
    Driver(String),

    /// The current platform/driver is unavailable.
    #[error("not supported: {0}")]
    NotSupported(String),

    #[error("invalid frame or parameter: {0}")]
    Invalid(String),

    #[cfg(any(
        feature = "vendor-advantech",
        feature = "vendor-can-analyst",
        feature = "vendor-eb-el",
        feature = "vendor-8devices",
        feature = "vendor-esd",
        feature = "vendor-etas",
        feature = "vendor-ime-actia",
        feature = "vendor-intrepid",
        feature = "vendor-ixxat",
        feature = "vendor-kvaser",
        feature = "vendor-lawicel",
        feature = "vendor-mhs",
        feature = "vendor-ni",
        feature = "vendor-peak",
        feature = "vendor-tosun",
        feature = "vendor-vector"
    ))]
    #[error(transparent)]
    Load(#[from] libloading::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
