use crate::{Machine, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// An address in a device profile: a bare number (`0x420090c0`) or an ELF
/// symbol expression (`&esp_app_desc.app_elf_sha256`). Symbol forms are
/// resolved through the debug transport at run time, so profiles survive
/// rebuilds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AddrExpr {
    Num(u64),
    Expr(String),
}

impl AddrExpr {
    /// Resolve to a numeric address. String forms are parsed first, then
    /// evaluated by the target as a GDB-style expression.
    pub fn resolve(&self, machine: &mut dyn Machine) -> Result<u64> {
        match self {
            AddrExpr::Num(n) => Ok(*n),
            AddrExpr::Expr(s) => {
                let t = s.trim();
                if let Some(h) = t.strip_prefix("0x") {
                    if let Ok(v) = u64::from_str_radix(h, 16) {
                        return Ok(v);
                    }
                }
                if let Ok(v) = t.parse::<u64>() {
                    return Ok(v);
                }
                machine.evaluate(&format!("(unsigned long)({t})"))?.as_u64()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub endian: Endian,
    pub image: Image,
    /// Optional: serial-only or capture/write-only targets may omit it.
    #[serde(default)]
    pub published: Option<Published>,
    #[serde(default)]
    pub power: Option<Power>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    pub stops: HashMap<String, Stop>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    #[default]
    Little,
    Big,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
    #[serde(default)]
    pub elf_sha256: Option<String>,
    #[serde(default)]
    pub verify: Vec<Verify>,
    #[serde(default)]
    pub model: Option<Model>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verify {
    /// Address (number or expression) of live memory compared with the ELF.
    pub addr: AddrExpr,
    pub n: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    /// Address (number or expression) of the model region to hash.
    pub addr: AddrExpr,
    pub size: usize,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Published {
    /// GDB/transport expression for the input that identifies the sample.
    pub x: String,
    /// Expression for the completed-inference counter.
    pub count: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Power {
    /// argv of a power-cycle command; no shell is involved.
    #[serde(default)]
    pub cycle: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    /// Expression for this channel's published value.
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    /// Breakpoint address (number or ELF symbol expression).
    pub addr: AddrExpr,
    #[serde(default)]
    pub cond: Option<String>,
}

impl Device {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn stop(&self, name: &str) -> Result<&Stop> {
        self.stops
            .get(name)
            .ok_or_else(|| crate::ElabError::msg(format!("unknown stop '{name}'")))
    }

    pub fn channel_names(&self) -> Vec<String> {
        self.channels.iter().map(|c| c.name.clone()).collect()
    }
}
