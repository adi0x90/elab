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
    /// OpenOCD server lifecycle. When present, elab verifies an OpenOCD is
    /// listening (or starts one with `start: true`, locally or over ssh) and
    /// stops the instance it started after the run.
    #[serde(default)]
    pub openocd: Option<OpenOcdSpec>,
}

/// OpenOCD server management for an experiment.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenOcdSpec {
    /// Config arguments passed to openocd, e.g.
    /// ["board/esp32c6-builtin.cfg"] or ["-f", "target/esp32c6.cfg", "-c",
    /// "adapter speed 20000"]. Passed verbatim after `-c`-joins.
    pub config: Vec<String>,
    /// Host running OpenOCD. Omit for local. With `start: true` elab runs
    /// openocd over ssh on this host and tunnels the gdb/telnet ports back.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default = "default_gdb_port")]
    pub gdb_port: u16,
    #[serde(default = "default_telnet_port")]
    pub telnet_port: u16,
    /// Start OpenOCD automatically and stop it after the run. When false,
    /// elab only verifies a server is already listening.
    #[serde(default)]
    pub start: bool,
}

fn default_gdb_port() -> u16 {
    3333
}
fn default_telnet_port() -> u16 {
    4444
}

#[derive(Debug, Clone, Deserialize)]
pub struct GdbSpec {
    pub endpoint: String,
    pub elf: String,
    /// Wait bound for GDB/MI responses (stop/resume/eval), milliseconds.
    #[serde(default = "default_gdb_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_gdb_timeout_ms() -> u64 {
    10_000
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
    /// Execute this phase a fixed number of times, in one transport session.
    #[serde(default = "default_repeat", deserialize_with = "repeat_count")]
    pub repeat: usize,
    pub ops: Vec<Op>,
}

fn default_repeat() -> usize {
    1
}

fn repeat_count<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    let count = usize::deserialize(d)?;
    if count == 0 {
        return Err(serde::de::Error::custom("phase repeat must be at least 1"));
    }
    Ok(count)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Halt without resetting or waiting for a breakpoint.
    Halt,
    /// Resume execution and return once the transport acknowledges running.
    Resume,
    /// Wait on the host; leave the target in its current running/halted state.
    Sleep {
        ms: u64,
    },
    /// Stop on one hardware write-watchpoint value change, then remove the watch.
    Watch {
        expr: String,
    },
    /// Record the current stack using the debug transport.
    Backtrace,
    Stop {
        at: String,
        #[serde(default)]
        when: Option<String>,
        /// Bound this stop's wait in milliseconds (overrides the transport).
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    Capture {
        name: String,
        expr: String,
    },
    Write {
        at: String,
        kind: String,
        /// Single scalar value.
        #[serde(default)]
        value: Option<String>,
        /// Bulk write: values are encoded with `kind` and concatenated,
        /// saved/restored and read back as one block.
        #[serde(default)]
        values: Option<Vec<String>>,
    },
    /// Write a CPU register from a GDB expression. `name` may include the
    /// leading `$`; `${var}` is substituted in `value`. Use this for a
    /// function argument that a memory write cannot reach.
    WriteReg {
        name: String,
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
        /// Bound each stop/wait in milliseconds (overrides the transport).
        #[serde(default)]
        timeout_ms: Option<u64>,
        /// Inline sample definition overriding the profile's [published].
        #[serde(default)]
        x: Option<String>,
        #[serde(default)]
        count: Option<String>,
        #[serde(default)]
        channels: Option<Vec<InlineChannel>>,
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
    /// Discard buffered serial input not yet consumed by serial_expect.
    SerialFlush,
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
    /// Read raw flash through OpenOCD's flash driver (works even when the
    /// CPU is halted with the cache off) and record it as an attachment.
    FlashRead {
        name: String,
        /// Flash offset: number or ELF symbol expression.
        offset: String,
        len: usize,
    },
}

/// A channel defined inline in a `published` op instead of the profile.
#[derive(Debug, Clone, Deserialize)]
pub struct InlineChannel {
    pub name: String,
    pub value: String,
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
