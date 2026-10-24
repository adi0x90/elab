//! Core model for elab: experiment specs, device profiles, phases, evidence.

pub mod device;
pub mod evidence;
pub mod image;
pub mod runner;
pub mod spec;
pub mod value;

pub use device::Device;
pub use evidence::Evidence;
pub use runner::{run, PublishedRow, RunResult};
pub use spec::{ExperimentSpec, Op, Phase, Transport};
pub use value::Value;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ElabError {
    #[error("{0}")]
    Msg(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("machine: {0}")]
    Machine(String),
    #[error("serial: {0}")]
    Serial(String),
}

impl ElabError {
    pub fn msg(s: impl Into<String>) -> Self {
        Self::Msg(s.into())
    }
}

pub type Result<T> = std::result::Result<T, ElabError>;

/// A debug target: stops, typed reads/writes, memory access, reset.
pub trait Machine {
    fn stop(&mut self, addr: u64, cond: Option<&str>) -> Result<u64>;
    fn evaluate(&mut self, expr: &str) -> Result<Value>;
    fn read_memory(&mut self, addr: u64, len: usize) -> Result<Vec<u8>>;
    fn write_memory(&mut self, addr: u64, bytes: &[u8]) -> Result<()>;
    fn reset(&mut self, halt: bool) -> Result<()>;
    fn detach(&mut self) -> Result<()>;
}

/// A serial/UART link used for witnesses and console-driven phases.
pub trait SerialLink {
    fn expect(&mut self, pattern: &str, timeout: std::time::Duration) -> Result<String>;
    fn send(&mut self, text: &str) -> Result<()>;
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

pub fn hex_bytes(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// A machine with no debug transport. Used by serial-only experiments; any
/// attempt to stop, read, write or reset returns an error.
pub struct NoMachine;

impl Machine for NoMachine {
    fn stop(&mut self, _addr: u64, _cond: Option<&str>) -> Result<u64> {
        Err(ElabError::msg("no debug transport in this experiment"))
    }
    fn evaluate(&mut self, _expr: &str) -> Result<Value> {
        Err(ElabError::msg("no debug transport in this experiment"))
    }
    fn read_memory(&mut self, _addr: u64, _len: usize) -> Result<Vec<u8>> {
        Err(ElabError::msg("no debug transport in this experiment"))
    }
    fn write_memory(&mut self, _addr: u64, _bytes: &[u8]) -> Result<()> {
        Err(ElabError::msg("no debug transport in this experiment"))
    }
    fn reset(&mut self, _halt: bool) -> Result<()> {
        Err(ElabError::msg("no debug transport in this experiment"))
    }
    fn detach(&mut self) -> Result<()> {
        Ok(())
    }
}
