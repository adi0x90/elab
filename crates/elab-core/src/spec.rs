use crate::{ElabError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentSpec {
    pub name: String,
    pub device: String,
    pub transport: Transport,
    #[serde(default)]
    pub phases: Vec<Phase>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Parameter>,
    #[serde(default)]
    pub measurements: BTreeMap<String, Vec<Op>>,
    #[serde(skip)]
    pub resolved_parameters: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeSpec {
    /// probe-rs target name, for example "esp32c6".
    pub chip: String,
    /// Optional ELF for the identity checks.
    #[serde(default)]
    pub elf: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialSpec {
    pub port: String,
    pub baud: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Invoke an ordered, reusable measurement sequence.
    Measure {
        name: String,
    },
    /// Restore the exact bytes saved by an active named write.
    Restore {
        write: String,
    },
    /// Compare recorded observations, never evaluate expressions on the target.
    Compare {
        left: String,
        right: String,
        #[serde(default)]
        relation: Relation,
        #[serde(default)]
        tolerance: f64,
    },
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
        #[serde(default)]
        id: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub kind: String,
    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    #[default]
    Equal,
    NotEqual,
    Greater,
}

impl ExperimentSpec {
    pub fn load_with_parameters(path: &Path, overrides: &[String]) -> Result<Self> {
        let spec: Self = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;
        spec.prepare(overrides)
    }

    /// Resolve typed inputs and expand reusable sequences without touching a target.
    pub fn prepare(&self, overrides: &[String]) -> Result<Self> {
        let mut inputs = BTreeMap::new();
        for arg in overrides {
            let (name, value) = arg
                .split_once('=')
                .ok_or_else(|| ElabError::msg("--set requires name=value"))?;
            if !self.parameters.contains_key(name) {
                return Err(ElabError::msg(format!("unknown parameter '{name}'")));
            }
            if inputs.insert(name.to_string(), value.to_string()).is_some() {
                return Err(ElabError::msg(format!("duplicate parameter '{name}'")));
            }
        }
        let mut resolved = self.resolved_parameters.clone();
        for (name, param) in &self.parameters {
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(ElabError::msg(format!("invalid parameter name '{name}'")));
            }
            let raw = inputs
                .get(name)
                .cloned()
                .or_else(|| {
                    param.default.as_ref().and_then(|v| match v {
                        serde_yaml::Value::String(s) => Some(s.clone()),
                        serde_yaml::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                })
                .ok_or_else(|| {
                    ElabError::msg(format!(
                        "parameter '{name}' needs a scalar default or --set {name}=value"
                    ))
                })?;
            resolved.insert(
                name.clone(),
                parameter_literal(&param.kind, &raw)
                    .map_err(|e| ElabError::msg(format!("parameter '{name}': {e}")))?,
            );
        }
        let mut spec = self.clone();
        // Parameter replacement happens in string values, never by textual YAML
        // replacement. A value cannot introduce additional operations or keys.
        spec.parameters.clear();
        let mut tree = serde_yaml::to_value(&spec)?;
        fn replace(v: &mut serde_yaml::Value, params: &BTreeMap<String, String>) {
            match v {
                serde_yaml::Value::String(s) => {
                    for (k, value) in params {
                        *s = s.replace(&format!("${{{k}}}"), value);
                    }
                }
                serde_yaml::Value::Sequence(xs) => {
                    for x in xs {
                        replace(x, params);
                    }
                }
                serde_yaml::Value::Mapping(m) => {
                    for (_, x) in m {
                        replace(x, params);
                    }
                }
                _ => (),
            }
        }
        replace(&mut tree, &resolved);
        spec = serde_yaml::from_value(tree)?;
        spec.resolved_parameters = resolved;
        fn expand(
            ops: &[Op],
            defs: &BTreeMap<String, Vec<Op>>,
            stack: &mut Vec<String>,
        ) -> Result<Vec<Op>> {
            let mut out = Vec::new();
            for op in ops {
                if let Op::Measure { name } = op {
                    if stack.contains(name) {
                        return Err(ElabError::msg(format!("recursive measurement '{name}'")));
                    }
                    let steps = defs
                        .get(name)
                        .ok_or_else(|| ElabError::msg(format!("unknown measurement '{name}'")))?;
                    stack.push(name.clone());
                    out.extend(expand(steps, defs, stack)?);
                    stack.pop();
                } else {
                    out.push(op.clone());
                }
                if out.len() > 100_000 {
                    return Err(ElabError::msg(
                        "expanded measurement exceeds 100000 operations",
                    ));
                }
            }
            Ok(out)
        }
        let mut names = std::collections::HashSet::new();
        for phase in &mut spec.phases {
            if !names.insert(phase.name.clone()) {
                return Err(ElabError::msg(format!("duplicate phase '{}'", phase.name)));
            }
            if phase.repeat == 0 {
                return Err(ElabError::msg("phase repeat must be at least 1"));
            }
            phase.ops = expand(&phase.ops, &spec.measurements, &mut Vec::new())?;
            for op in &phase.ops {
                if let Op::Compare { tolerance, .. } = op {
                    if !tolerance.is_finite() || *tolerance < 0.0 {
                        return Err(ElabError::msg(
                            "comparison tolerance must be finite and nonnegative",
                        ));
                    }
                }
            }
        }
        spec.measurements.clear();
        Ok(spec)
    }
}

fn parameter_literal(kind: &str, raw: &str) -> Result<String> {
    let raw = raw.trim();
    let bad = || ElabError::msg(format!("'{raw}' is not a valid {kind}"));
    match kind {
        "address" | "u8" | "u32" | "u64" => {
            let n = if let Some(h) = raw.strip_prefix("0x") {
                u64::from_str_radix(h, 16)
            } else {
                raw.parse()
            }
            .map_err(|_| bad())?;
            let max = match kind {
                "u8" => u8::MAX as u64,
                "u32" => u32::MAX as u64,
                _ => u64::MAX,
            };
            if n > max {
                return Err(bad());
            }
            Ok(n.to_string())
        }
        "i8" | "i32" | "i64" => {
            let n: i64 = raw.parse().map_err(|_| bad())?;
            let (min, max) = match kind {
                "i8" => (i8::MIN as i64, i8::MAX as i64),
                "i32" => (i32::MIN as i64, i32::MAX as i64),
                _ => (i64::MIN, i64::MAX),
            };
            if n < min || n > max {
                return Err(bad());
            }
            Ok(n.to_string())
        }
        "f32" | "f64" => {
            let n: f64 = raw.parse().map_err(|_| bad())?;
            if !n.is_finite() || (kind == "f32" && !(n as f32).is_finite()) {
                return Err(bad());
            }
            Ok(format!("{n:?}"))
        }
        _ => Err(ElabError::msg(format!("unknown parameter kind '{kind}'"))),
    }
}
