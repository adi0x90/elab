use crate::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentSpec {
    pub name: String,
    pub device: String,
    pub transport: Transport,
    #[serde(default)]
    pub phases: Vec<Phase>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Transport {
    #[serde(default)]
    pub gdb: Option<GdbSpec>,
    #[serde(default)]
    pub probe: Option<ProbeSpec>,
    #[serde(default)]
    pub serial: Option<SerialSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GdbSpec {
    pub endpoint: String,
    pub elf: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProbeSpec {
    /// probe-rs target name, for example "esp32c6".
    pub chip: String,
    /// Optional ELF for the identity checks.
    #[serde(default)]
    pub elf: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SerialSpec {
    pub port: String,
    pub baud: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Phase {
    pub name: String,
    pub ops: Vec<Op>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Stop {
        at: String,
        #[serde(default)]
        when: Option<String>,
    },
    Capture {
        name: String,
        expr: String,
    },
    Write {
        at: String,
        kind: String,
        value: String,
    },
    Published {
        stop: String,
        #[serde(rename = "match")]
        match_expr: String,
        #[serde(default = "default_tol")]
        tol: f64,
        #[serde(default = "default_tries")]
        tries: usize,
    },
    Expect {
        fields: HashMap<String, f64>,
    },
    SerialExpect {
        pattern: String,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
    SerialSend {
        text: String,
    },
    /// Reset the core through the debug transport. `halt: true` keeps it halted.
    Reset {
        #[serde(default)]
        halt: bool,
    },
    /// Run the device profile's [power] cycle command (argv, no shell).
    PowerCycle,
    /// Read memory and write it to runs/<run>/attachments/<name>.bin.
    Attachment {
        name: String,
        at: String,
        len: usize,
    },
}

fn default_tol() -> f64 {
    1e-4
}
fn default_tries() -> usize {
    30
}
fn default_timeout_ms() -> u64 {
    5000
}

impl ExperimentSpec {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }
}
