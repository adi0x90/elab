use elab_core::{run, Device, ElabError, Evidence, ExperimentSpec, Machine, Result, Value};

struct Target {
    memory: Vec<u8>,
    writes: usize,
    fail_write: bool,
    /// Corrupt the first byte on a successful write to trigger a
    /// "did not stick" rollback; false for clean-write scenarios.
    corrupt: bool,
}

impl Machine for Target {
    fn stop(&mut self, addr: u64, _: Option<&str>) -> Result<u64> {
        Ok(addr)
    }
    fn evaluate(&mut self, expr: &str) -> Result<Value> {
        Ok(Value::UInt(expr.parse().unwrap_or(1)))
    }
    fn read_memory(&mut self, _: u64, _: usize) -> Result<Vec<u8>> {
        Ok(self.memory.clone())
    }
    fn write_memory(&mut self, _: u64, bytes: &[u8]) -> Result<()> {
        self.writes += 1;
        self.memory = bytes.to_vec();
        if self.writes == 1 && self.fail_write {
            self.memory[0] = 99;
            return Err(ElabError::msg("partial write"));
        }
        if self.writes == 1 && self.corrupt {
            self.memory[0] = 99;
        }
        Ok(())
    }
    fn reset(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
    fn detach(&mut self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn restores_original_bytes_after_failed_write_or_readback() {
    let device: Device = toml::from_str("name = 'mock'\n[image]\n[stops]\n").unwrap();
    let spec: ExperimentSpec = serde_yaml::from_str("name: rollback\ndevice: mock.toml\ntransport: {}\nphases:\n  - name: change\n    ops:\n      - { op: write, at: '0x1000', kind: u8, value: '2' }\n").unwrap();
    for fail_write in [false, true] {
        let base =
            std::env::temp_dir().join(format!("elab-rollback-{}-{fail_write}", std::process::id()));
        let mut ev = Evidence::create(&base, "test").unwrap();
        let mut target = Target {
            memory: vec![7],
            writes: 0,
            fail_write,
            corrupt: true,
        };
        assert!(run(&spec, &device, &mut target, None, &mut ev).is_err());
        assert_eq!(target.memory, vec![7], "original bytes must be restored");
        drop(ev);
        std::fs::remove_dir_all(base).unwrap();
    }
}

/// Bulk write encodes values in order, writes one block, and restores the
/// whole block on failure.
#[test]
fn bulk_write_writes_one_block_and_restores_all() {
    let device: Device = toml::from_str("name = 'mock'\n[image]\n[stops]\n").unwrap();
    let spec: ExperimentSpec = serde_yaml::from_str(
        "name: bulk\ndevice: mock.toml\ntransport: {}\nphases:\n  - name: change\n    ops:\n      - { op: write, at: '0x2000', kind: u8, values: ['1', '2', '3', '4'] }\n",
    )
    .unwrap();
    for fail_write in [false, true] {
        let base =
            std::env::temp_dir().join(format!("elab-bulk-{}-{fail_write}", std::process::id()));
        let mut ev = Evidence::create(&base, "test").unwrap();
        let mut target = Target {
            memory: vec![9, 9, 9, 9],
            writes: 0,
            fail_write,
            corrupt: false,
        };
        let result = run(&spec, &device, &mut target, None, &mut ev);
        assert_eq!(result.is_err(), fail_write);
        if fail_write {
            assert_eq!(target.memory, vec![9, 9, 9, 9], "block restored on failure");
            assert_eq!(target.writes, 2, "one failed write plus one restore");
        } else {
            assert_eq!(target.memory, vec![1, 2, 3, 4], "bytes written on success");
            assert_eq!(target.writes, 1, "values array writes as one block");
        }
        drop(ev);
        std::fs::remove_dir_all(base).unwrap();
    }
}

/// A write spec with both value and values, or neither, is an error.
#[test]
fn write_rejects_ambiguous_and_empty_specs() {
    let device: Device = toml::from_str("name = 'mock'\n[image]\n[stops]\n").unwrap();
    for yaml in [
        "name: x\ndevice: mock.toml\ntransport: {}\nphases:\n  - name: p\n    ops:\n      - { op: write, at: '0x2000', kind: u8, value: '1', values: ['1'] }\n",
        "name: x\ndevice: mock.toml\ntransport: {}\nphases:\n  - name: p\n    ops:\n      - { op: write, at: '0x2000', kind: u8 }\n",
    ] {
        let spec: ExperimentSpec = serde_yaml::from_str(yaml).unwrap();
        let base = std::env::temp_dir().join(format!("elab-writebad-{}", std::process::id()));
        let mut ev = Evidence::create(&base, "test").unwrap();
        let mut target = Target {
            memory: vec![1],
            writes: 0,
            fail_write: false,
            corrupt: false,
        };
        assert!(run(&spec, &device, &mut target, None, &mut ev).is_err());
        drop(ev);
        std::fs::remove_dir_all(base).unwrap();
    }
}
