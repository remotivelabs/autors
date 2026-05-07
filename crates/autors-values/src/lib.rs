//! autors-values: A2L calibration value objects (runtime values of
//! CHARACTERISTICs in their various CHAR_TYPE shapes).
//! COMPU_METHOD formula evaluation lives in autors-formula; the CDF, DCM and
//! data-file formats live in autors-cdf, autors-dcm and autors-datafile
//! respectively.

pub mod cdf_link;
pub mod ecu_io;
pub mod error;
pub mod value;

pub use error::{Error, Result};
