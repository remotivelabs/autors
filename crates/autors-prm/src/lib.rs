//! autors-prm: parsing and execution of INCA ProF-style PRM/CNF flash scripts.
//! - [`prm`]: parsing of `.prm` scripts (plus `.pri` sub-procedures and the
//!   referenced `.cnf` controller configuration) into [`prm::PrmFile`] /
//!   [`prm::CnfFile`], including `SOURCE_MEM_AREA` segment data loading.
//! - [`prm_if`]: the execution interface — the [`prm_if::Prm`] process entry
//!   point, the [`prm_if::PrmExecutor`] instruction set, and the
//!   [`prm_if::PrmMsg`] message type.
//! - [`executor`]: the [`executor::Executor`] flash executor, which runs the
//!   script instructions through injected UDS/CCP/XCP clients.
//! - [`blocking`] (feature `blocking`): synchronous facade over the async
//!   executor core.

#[cfg(feature = "blocking")]
pub mod blocking;
pub mod error;
pub mod executor;
pub mod prm;
pub mod prm_if;
pub mod script;

pub use error::{Error, Result};
