//! Error types for A2L parsing and writing.

/// Unified error type for A2L parse and write operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Parsing failed; carries the line number and context.
    #[error("parse error at line {line}: {message}")]
    Parse {
        /// Line where the error occurred (1-based; 0 when the failure happened
        /// during lexing, before any line context existed).
        line: u32,
        /// Error description.
        message: String,
    },

    /// Writing failed.
    #[error("write error in node {node}: {message}")]
    Write {
        /// Name of the node type being written when the error occurred.
        node: &'static str,
        /// Error description.
        message: String,
    },

    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Constructs a parse error with a line number.
    pub(crate) fn parse(line: u32, message: impl Into<String>) -> Self {
        Error::Parse {
            line,
            message: message.into(),
        }
    }

    /// Constructs a write error.
    pub(crate) fn write(node: &'static str, message: impl Into<String>) -> Self {
        Error::Write {
            node,
            message: message.into(),
        }
    }
}
