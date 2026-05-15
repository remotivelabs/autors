//! Real serial-port wiring for ELM327 adapters: a thin [`Elm327Io`] shell over
//! the serialport crate.
//! The protocol codec lives in [`super::elm327`] (generic over [`Elm327Io`]);
//! this module only supplies the two concrete pieces:
//! - [`SerialPortElmIo`]: an [`Elm327Io`] adapter over a real serial port from
//!   the serialport crate;
//! - [`open_elm327_io`] / [`open_elm327`]: open the serial port and wrap it.
//!   The port is configured as `SerialPort(comPort, baudRate, Parity.None, 8,
//!   StopBits.One)`, with the read/write timeouts set to the ELM327 command
//!   timeout.
//!
//! Note: there is no serialport API for sizing the read/write buffers (64 KB
//! would be typical), so the OS default buffering is used.

use std::io::{Read as _, Write as _};

use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

use super::elm327::{open_failed_msg, parse_com_port_baud, Elm327Can, Elm327Io};
use crate::error::{Error, Result};
use crate::frame::CanConfiguration;

/// Concrete [`Elm327Io`] implementation over a serial port (serialport crate).
/// `is_open` is true while the port is open; it is set to false after a
/// non-timeout IO error on read or write (mirroring how a serial port reports
/// itself closed after the connection drops unexpectedly).
pub struct SerialPortElmIo {
    inner: Box<dyn SerialPort>,
    open: bool,
}

impl SerialPortElmIo {
    pub fn new(inner: Box<dyn SerialPort>) -> Self {
        Self { inner, open: true }
    }

    pub fn inner(&self) -> &dyn SerialPort {
        &*self.inner
    }
}

// TODO(hardware): route serial waits through spawn_blocking if executor stall becomes an issue
impl Elm327Io for SerialPortElmIo {
    fn is_open(&self) -> bool {
        self.open
    }

    fn bytes_to_read(&mut self) -> usize {
        self.inner.bytes_to_read().unwrap_or(0) as usize
    }

    fn read(&mut self, buf: &mut [u8]) -> usize {
        match self.inner.read(buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => 0,
            Err(_) => {
                self.open = false;
                0
            }
        }
    }

    fn write(&mut self, data: &[u8]) {
        if self.inner.write_all(data).is_err() {
            self.open = false;
        }
    }

    fn discard_buffers(&mut self) {
        let _ = self.inner.clear(serialport::ClearBuffer::All);
    }
}

pub fn open_elm327_io(
    com_port: &str,
    com_port_config: &str,
    cmd_timeout_ms: i32,
) -> Result<SerialPortElmIo> {
    let failed = |e: Error| {
        Error::Driver(format!(
            "{}: {e}",
            open_failed_msg(com_port, com_port_config)
        ))
    };
    let baud = parse_com_port_baud(com_port_config).map_err(&failed)?;
    let timeout = std::time::Duration::from_millis(cmd_timeout_ms.max(0) as u64);
    let port = serialport::new(com_port, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(timeout)
        .open()
        .map_err(|e| failed(Error::Driver(e.to_string())))?;
    Ok(SerialPortElmIo::new(port))
}

pub fn open_elm327(config: &CanConfiguration) -> Result<Elm327Can<SerialPortElmIo>> {
    let com_port = config
        .com_port
        .as_deref()
        .ok_or_else(|| Error::Driver(open_failed_msg("", &config.com_port_config)))?;
    let io = open_elm327_io(com_port, &config.com_port_config, config.elm327_cmd_timeout)?;
    Ok(Elm327Can::new(io))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_elm327_io_bad_port_returns_expected_error() {
        let err = open_elm327_io("AUTORS_NONEXISTENT_PORT_0", "9600,8,N,1", 200)
            .err()
            .expect("expected open to fail");
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to open AUTORS_NONEXISTENT_PORT_0 (9600,8,N,1)"),
            "got: {msg}"
        );
    }

    #[test]
    fn open_elm327_io_bad_baud_fails() {
        let err = open_elm327_io("COM1", "not_a_number,8,N,1", 200)
            .err()
            .expect("expected open to fail");
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to open COM1 (not_a_number,8,N,1)"),
            "got: {msg}"
        );
    }

    #[test]
    fn open_elm327_requires_com_port() {
        let config = CanConfiguration::default();
        let err = open_elm327(&config).err().expect("expected open to fail");
        assert!(err.to_string().contains("Failed to open"), "got: {err}");
    }

    #[test]
    fn open_elm327_bad_port_fails() {
        let config = CanConfiguration::with_com_port("AUTORS_NONEXISTENT_PORT_0", true, 200, 6);
        let err = open_elm327(&config).err().expect("expected open to fail");
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to open AUTORS_NONEXISTENT_PORT_0"),
            "got: {msg}"
        );
    }
}
