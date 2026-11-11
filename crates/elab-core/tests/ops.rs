use elab_core::{
    run, Device, ElabError, Evidence, ExperimentSpec, Machine, Op, Phase, Result, Transport, Value,
};
use std::collections::HashMap;

struct Mock {
    read: Vec<u8>,
    reset_halt: Option<bool>,
    regs: Vec<(String, String)>,
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
    fn write_register(&mut self, name: &str, value: &str) -> Result<()> {
        self.regs.push((name.to_string(), value.to_string()));
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
            repeat: 1,
            name: "ops".into(),
            ops: vec![
                Op::Reset { halt: true },
                Op::PowerCycle,
                Op::Attachment {
                    name: "dump".into(),
                    at: "0x2000".into(),
                    len: 4,
                },
                Op::WriteReg {
                    name: "a0".into(),
                    value: "0x3f666666".into(),
                },
            ],
        }],
    };

    let mut mock = Mock {
        read: vec![0xAA, 0xBB],
        reset_halt: None,
        regs: Vec::new(),
    };
    let dir = std::env::temp_dir().join(format!("elab-ops-{}", std::process::id()));
    let mut ev = Evidence::create(&dir, "ops").unwrap();
    let result = run(&spec, &device, &mut mock, None, &mut ev).unwrap();
    assert!(result.ok);
    assert_eq!(mock.reset_halt, Some(true));

    let dump = ev.dir().join("attachments/dump.bin");
    assert_eq!(std::fs::read(&dump).unwrap(), vec![0xAA, 0xBB, 0xAA, 0xBB]);
    assert_eq!(
        mock.regs,
        vec![("a0".to_string(), "0x3f666666".to_string())]
    );

    let _ = ev.finish(true, serde_json::json!({}));
    let _ = std::fs::remove_dir_all(dir);
}

/// A profile with no [published] still supports published ops when x, count
/// and channels are all defined inline.
#[test]
fn inline_published_channels_override_profile() {
    let device: Device = toml::from_str(
        r#"
name = "mock"
[image]
[stops]
sample = { addr = 0x3000 }
"#,
    )
    .unwrap();

    let spec = ExperimentSpec {
        name: "inline".into(),
        device: "mock.toml".into(),
        transport: Transport::default(),
        phases: vec![Phase {
            repeat: 1,
            name: "p".into(),
            ops: vec![
                Op::Published {
                    stop: "sample".into(),
                    match_expr: "0x10".into(),
                    tol: 1e-4,
                    tries: 3,
                    timeout_ms: None,
                    x: Some("0x10".into()),
                    count: Some("0x11".into()),
                    channels: Some(vec![
                        elab_core::spec::InlineChannel {
                            name: "alpha".into(),
                            value: "0x12".into(),
                        },
                        elab_core::spec::InlineChannel {
                            name: "beta".into(),
                            value: "0x13".into(),
                        },
                    ]),
                },
                Op::Expect {
                    fields: HashMap::from([
                        ("alpha".to_string(), 18.0),
                        ("beta".to_string(), 19.0),
                    ]),
                },
            ],
        }],
    };

    let mut mock = Mock {
        read: vec![],
        reset_halt: None,
        regs: Vec::new(),
    };
    let dir = std::env::temp_dir().join(format!("elab-inline-{}", std::process::id()));
    let mut ev = Evidence::create(&dir, "inline").unwrap();
    let result = run(&spec, &device, &mut mock, None, &mut ev).unwrap();
    assert!(result.ok);
    assert_eq!(
        result.channels,
        vec!["alpha".to_string(), "beta".to_string()]
    );
    assert_eq!(result.rows.len(), 1);
    let _ = ev.finish(true, serde_json::json!({}));
    let _ = std::fs::remove_dir_all(dir);
}

/// A profile with symbol-expression stops resolves them through the machine.
#[test]
fn symbol_stop_addresses_resolve_via_machine() {
    use std::collections::HashMap;
    struct SymbolMock {
        expect_expr: String,
    }
    impl Machine for SymbolMock {
        fn stop(&mut self, addr: u64, _: Option<&str>) -> Result<u64> {
            Ok(addr)
        }
        fn evaluate(&mut self, expr: &str) -> Result<Value> {
            self.expect_expr = expr.to_string();
            if expr.contains("(unsigned long)") {
                return Ok(Value::UInt(0x4200_9170));
            }
            Ok(Value::UInt(0))
        }
        fn read_memory(&mut self, _: u64, _: usize) -> Result<Vec<u8>> {
            Ok(vec![0])
        }
        fn write_memory(&mut self, _: u64, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn reset(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn detach(&mut self) -> Result<()> {
            Ok(())
        }
    }
    let device: Device = toml::from_str(
        r#"
name = "mock"
[image]
[stops]
measure = { addr = "&sensor_task" }
"#,
    )
    .unwrap();
    let spec = ExperimentSpec {
        name: "symbol".into(),
        device: "mock.toml".into(),
        transport: Transport::default(),
        phases: vec![Phase {
            repeat: 1,
            name: "p".into(),
            ops: vec![Op::Stop {
                at: "measure".into(),
                when: None,
                timeout_ms: None,
            }],
        }],
    };
    let mut mock = SymbolMock {
        expect_expr: String::new(),
    };
    let dir = std::env::temp_dir().join(format!("elab-symbol-{}", std::process::id()));
    let mut ev = Evidence::create(&dir, "symbol").unwrap();
    run(&spec, &device, &mut mock, None, &mut ev).unwrap();
    assert!(
        mock.expect_expr.contains("(unsigned long)"),
        "symbol address must be evaluated as an expression, got {:?}",
        mock.expect_expr
    );
    let _ = ev.finish(true, serde_json::json!({}));
    let _ = std::fs::remove_dir_all(dir);
}
