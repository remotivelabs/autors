//! Calibration data conservation file formats: DCM (Data Conservation /
//! Konservierungsformat, DAMOS), MATLAB `.m`, and CANape PAR.
//! Core types live in [`conservation`]: the `DataConservation` base container
//! together with the `DcmFile`, `MatlabFile`, and `ParFile` format handlers for
//! reading and writing calibration values. The error type is defined in
//! [`error`] and re-exported at the crate root.

pub mod conservation;
pub mod error;

pub use error::{Error, Result};
