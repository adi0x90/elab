use crate::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    #[default]
    Little,
    Big,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Image {
    #[serde(default)]
    pub elf_sha256: Option<String>,
    #[serde(default)]
    pub verify: Vec<Verify>,
    #[serde(default)]
    pub model: Option<Model>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Verify {
    pub addr: u64,
    pub n: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub addr: u64,
    pub size: usize,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Published {
    /// GDB/transport expression for the input that identifies the sample.
    pub x: String,
    /// Expression for the completed-inference counter.
    pub count: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Power {
    /// argv of a power-cycle command; no shell is involved.
    #[serde(default)]
    pub cycle: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub name: String,
    /// Expression for this channel's published value.
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Stop {
    pub addr: u64,
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
