use crate::Result;
use serde_json::{json, Value as Json};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Evidence {
    dir: PathBuf,
    file: File,
    pub events: Vec<Json>,
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

impl Evidence {
    pub fn create(base: &Path, name: &str) -> Result<Self> {
        let dir = base.join(format!("{name}-{}", epoch_ms()));
        std::fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        Ok(Self {
            dir,
            file,
            events: Vec::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn event(&mut self, mut v: Json) -> Result<()> {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("ts_ms".into(), json!(epoch_ms()));
        }
        writeln!(self.file, "{}", serde_json::to_string(&v)?)?;
        self.file.flush()?;
        self.events.push(v);
        Ok(())
    }

    pub fn finish(self, ok: bool, mut manifest: Json) -> Result<PathBuf> {
        if let Some(obj) = manifest.as_object_mut() {
            obj.insert("ok".into(), json!(ok));
            obj.insert("events".into(), json!(self.events.len()));
        }
        let path = self.dir.join("manifest.json");
        std::fs::write(&path, serde_json::to_string_pretty(&manifest)?)?;
        Ok(self.dir.clone())
    }
}
