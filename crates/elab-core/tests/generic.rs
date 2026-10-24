use elab_core::{
    run, Device, ElabError, Evidence, ExperimentSpec, Machine, Op, Phase, Result, Transport, Value,
};
use std::collections::HashMap;

struct Mock {
    vals: HashMap<String, f64>,
}

impl Machine for Mock {
    fn stop(&mut self, addr: u64, _cond: Option<&str>) -> Result<u64> {
        Ok(addr)
    }
    fn evaluate(&mut self, expr: &str) -> Result<Value> {
        if let Some(v) = self.vals.get(expr) {
            return Ok(Value::Float(*v));
        }
        if let Ok(f) = expr.parse::<f64>() {
            return Ok(Value::Float(f));
        }
        Err(ElabError::msg(format!("mock has no {expr}")))
    }
    fn read_memory(&mut self, _addr: u64, len: usize) -> Result<Vec<u8>> {
        Ok(vec![0; len])
    }
    fn write_memory(&mut self, _addr: u64, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self, _halt: bool) -> Result<()> {
        Ok(())
    }
    fn detach(&mut self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn arbitrary_channels_and_big_endian() {
    let device: Device = toml::from_str(
        r#"
name = "mock"
endian = "big"
[image]
[published]
x = "s.x"
count = "s.count"
[[channels]]
name = "alpha"
value = "s.a"
[[channels]]
name = "beta"
value = "s.b"
[stops]
pub = { addr = 0x1000 }
"#,
    )
    .unwrap();

    let spec = ExperimentSpec {
        name: "mock".into(),
        device: "mock.toml".into(),
        transport: Transport::default(),
        phases: vec![Phase {
            name: "run".into(),
            ops: vec![
                Op::Capture {
                    name: "target".into(),
                    expr: "1.0".into(),
                },
                Op::Published {
                    stop: "pub".into(),
                    match_expr: "${target}".into(),
                    tol: 1e-6,
                    tries: 2,
                },
                Op::Expect {
                    fields: HashMap::from([
                        ("alpha".to_string(), 1.5),
                        ("beta".to_string(), 2.5),
                        ("count".to_string(), 7.0),
                    ]),
                },
            ],
        }],
    };

    let mut mock = Mock {
        vals: HashMap::from([
            ("s.x".to_string(), 1.0),
            ("s.count".to_string(), 7.0),
            ("s.a".to_string(), 1.5),
            ("s.b".to_string(), 2.5),
        ]),
    };

    let dir = std::env::temp_dir().join(format!("elab-generic-{}", std::process::id()));
    let mut ev = Evidence::create(&dir, "mock").unwrap();
    let result = run(&spec, &device, &mut mock, None, &mut ev).unwrap();

    assert_eq!(
        result.channels,
        vec!["alpha".to_string(), "beta".to_string()]
    );
    assert_eq!(result.rows[0].values["alpha"], 1.5);
    assert!(result.ok);

    let _ = ev.finish(true, serde_json::json!({}));
    let _ = std::fs::remove_dir_all(dir);
}
