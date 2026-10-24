//! Serial/UART transport for elab.

use elab_core::{ElabError, Result, SerialLink};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

pub struct Serial {
    port: Box<dyn serialport::SerialPort>,
    buf: Vec<u8>,
}

impl Serial {
    pub fn open(port: &str, baud: u32) -> Result<Self> {
        let mut p = serialport::new(port, baud)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|e| ElabError::Serial(format!("open {port}: {e}")))?;
        // The C6 console resets on DTR/RTS transitions; leave both low.
        let _ = p.write_data_terminal_ready(false);
        let _ = p.write_request_to_send(false);
        Ok(Self {
            port: p,
            buf: Vec::new(),
        })
    }

    pub fn close(&mut self) {
        let _ = self.port.write_data_terminal_ready(false);
        let _ = self.port.write_request_to_send(false);
    }
}

impl SerialLink for Serial {
    fn expect(&mut self, pattern: &str, timeout: Duration) -> Result<String> {
        let start = Instant::now();
        let mut chunk = [0u8; 512];
        if pattern.is_empty() {
            return Err(ElabError::Serial("empty expect pattern".into()));
        }
        loop {
            if let Some(pos) = self
                .buf
                .windows(pattern.len())
                .position(|w| w == pattern.as_bytes())
            {
                let consumed: Vec<u8> = self.buf.drain(..pos + pattern.len()).collect();
                return Ok(String::from_utf8_lossy(&consumed).into_owned());
            }
            match self.port.read(&mut chunk) {
                Ok(n) if n > 0 => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    if start.elapsed() > timeout {
                        return Err(ElabError::Serial(format!(
                            "timeout waiting for {pattern:?}"
                        )));
                    }
                    continue;
                }
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(ElabError::Serial(format!("read: {e}"))),
            }
            if start.elapsed() > timeout {
                return Err(ElabError::Serial(format!(
                    "timeout waiting for {pattern:?}"
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, text: &str) -> Result<()> {
        self.port
            .write_all(text.as_bytes())
            .map_err(|e| ElabError::Serial(format!("write: {e}")))?;
        self.port
            .flush()
            .map_err(|e| ElabError::Serial(format!("flush: {e}")))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serialport::SerialPort;

    #[test]
    fn expect_consumes_matches_and_preserves_following_output() {
        let (mut peer, mut port) = serialport::TTYPort::pair().unwrap();
        port.set_timeout(Duration::from_millis(10)).unwrap();
        let mut serial = Serial {
            port: Box::new(port),
            buf: Vec::new(),
        };
        peer.write_all(b"READY\nDONE\n").unwrap();
        assert!(serial.expect("READY", Duration::from_millis(200)).is_ok());
        // DONE may already be buffered; it must not require another read.
        assert!(serial.expect("DONE", Duration::from_millis(200)).is_ok());
        peer.write_all(b"unrelated\n").unwrap();
        assert!(serial.expect("READY", Duration::from_millis(50)).is_err());
    }
}
