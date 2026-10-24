use elab_core::{
    run, Device, ElabError, Evidence, ExperimentSpec, Machine, Op, Phase, Result, Transport, Value,
};

struct Mock {
    read: Vec<u8>,
    reset_halt: Option<bool>,
}

impl Machine for Mock {
    fn stop(&mut self, addr: u64, _cond: Option<&str>) -> Result<u64> {
        Ok(addr)
    }
    fn evaluate(&mut self, expr: &str) -> Result<Value> {
        if let Some(h) = expr.trim().strip_prefix("0x") {
            if let Ok(v) = u64::from_str_radix(h, 16) {
                return Ok(Value::UInt(v));
            }
        }
        Err(ElabError::msg(format!("mock has no {expr}")))
    }
    fn read_memory(&mut self, _addr: u64, len: usize) -> Result<Vec<u8>> {
        Ok(self.read.iter().copied().cycle().take(len).collect())
    }
    fn write_memory(&mut self, _addr: u64, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self, halt: bool) -> Result<()> {
        self.reset_halt = Some(halt);
        Ok(())
    }
    fn detach(&mut self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn reset_power_and_attachment() {
    let device: Device = toml::from_str(
        r#"
name = "mock"
[image]
[power]
cycle = ["true"]
[stops]
entry = { addr = 0x1000 }
"#,
    )
    .unwrap();

    let spec = ExperimentSpec {
        name: "ops".into(),
        device: "mock.toml".into(),
        transport: Transport::default(),
        phases: vec![Phase {
            name: "ops".into(),
            ops: vec![
                Op::Reset { halt: true },
                Op::PowerCycle,
                Op::Attachment {
                    name: "dump".into(),
                    at: "0x2000".into(),
                    len: 4,
                },
            ],
        }],
    };

    let mut mock = Mock {
        read: vec![0xAA, 0xBB],
        reset_halt: None,
    };
    let dir = std::env::temp_dir().join(format!("elab-ops-{}", std::process::id()));
    let mut ev = Evidence::create(&dir, "ops").unwrap();
    let result = run(&spec, &device, &mut mock, None, &mut ev).unwrap();
    assert!(result.ok);
    assert_eq!(mock.reset_halt, Some(true));

    let dump = ev.dir().join("attachments/dump.bin");
    assert_eq!(std::fs::read(&dump).unwrap(), vec![0xAA, 0xBB, 0xAA, 0xBB]);

    let _ = ev.finish(true, serde_json::json!({}));
    let _ = std::fs::remove_dir_all(dir);
}
