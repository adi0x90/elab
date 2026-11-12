use elab_core::{run, Device, Evidence, ExperimentSpec, Machine, Result, Value, WatchHit};

#[test]
fn accepts_watch_operation() {
    let spec = serde_yaml::from_str::<ExperimentSpec>(
        r#"
name: watch
device: c6.toml
transport: {}
phases:
  - name: one
    ops:
      - {op: watch, expr: "nn_act[4]"}
"#,
    );
    assert!(spec.is_ok(), "watch operation rejected: {spec:?}");
}

#[test]
fn accepts_backtrace_operation() {
    let spec = serde_yaml::from_str::<ExperimentSpec>(
        r#"
name: stack
device: c6.toml
transport: {}
phases:
  - name: one
    ops:
      - {op: backtrace}
"#,
    );
    assert!(spec.is_ok(), "backtrace operation rejected: {spec:?}");
}

#[test]
fn watch_and_backtrace_preserve_evidence_alongside_symbol_stops_and_timeouts() {
    const RAW_HIT: &str = "*stopped,reason=\"watchpoint-trigger\",thread-id=\"2\"";
    const RAW_STACK: &str = "^done,stack=[frame={level=\"0\",func=\"nn_infer\"}]";

    #[derive(Default)]
    struct Mock {
        timeouts: Vec<u64>,
    }

    impl Machine for Mock {
        fn watch(&mut self, expr: &str) -> Result<WatchHit> {
            assert_eq!(expr, "nn_act[4]");
            Ok(WatchHit {
                pc: 0x1004,
                reason: "watchpoint-trigger".into(),
                old: "25".into(),
                new: "26".into(),
                raw: RAW_HIT.into(),
            })
        }
        fn backtrace(&mut self) -> Result<String> {
            Ok(RAW_STACK.into())
        }
        fn stop(&mut self, addr: u64, cond: Option<&str>) -> Result<u64> {
            assert_eq!(addr, 0x1000);
            assert_eq!(cond, Some("ready"));
            assert_eq!(self.timeouts.last(), Some(&25));
            Ok(addr)
        }
        fn evaluate(&mut self, expr: &str) -> Result<Value> {
            match expr {
                "4" => Ok(Value::UInt(4)),
                "(unsigned long)(&entry)" => Ok(Value::UInt(0x1000)),
                _ => panic!("unexpected expression: {expr}"),
            }
        }
        fn set_timeout_ms(&mut self, ms: u64) {
            self.timeouts.push(ms);
        }
        fn read_memory(&mut self, _addr: u64, _len: usize) -> Result<Vec<u8>> {
            panic!("unexpected memory read")
        }
        fn write_memory(&mut self, _addr: u64, _bytes: &[u8]) -> Result<()> {
            panic!("unexpected memory write")
        }
        fn reset(&mut self, _halt: bool) -> Result<()> {
            panic!("unexpected reset")
        }
        fn detach(&mut self) -> Result<()> {
            Ok(())
        }
    }

    let spec: ExperimentSpec = serde_yaml::from_str(
        r#"
name: watch-and-stop
device: mock.toml
transport: {}
phases:
  - name: observe
    ops:
      - {op: capture, name: index, expr: "4"}
      - {op: watch, expr: "nn_act[${index}]"}
      - {op: backtrace}
      - {op: stop, at: entry, when: ready, timeout_ms: 25}
"#,
    )
    .unwrap();
    let device: Device =
        toml::from_str("name = 'mock'\n[image]\n[stops]\nentry = { addr = '&entry' }\n").unwrap();
    let dir = std::env::temp_dir().join(format!("elab-watch-runner-{}", std::process::id()));
    let mut ev = Evidence::create(&dir, "watch-and-stop").unwrap();
    let mut mock = Mock::default();
    assert!(run(&spec, &device, &mut mock, None, &mut ev).unwrap().ok);
    assert_eq!(mock.timeouts, vec![25, 10_000]);

    let records: Vec<serde_json::Value> = std::fs::read_to_string(ev.dir().join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 6);
    assert_eq!(records[2]["event"], "observation");
    assert_eq!(records[3]["event"], "watch");
    assert_eq!(records[3]["expr"], "nn_act[4]");
    assert_eq!(records[3]["hit"]["pc"], 0x1004);
    assert_eq!(records[3]["hit"]["old"], "25");
    assert_eq!(records[3]["hit"]["new"], "26");
    assert_eq!(records[3]["hit"]["raw"], RAW_HIT);
    assert_eq!(records[4]["event"], "backtrace");
    assert_eq!(records[4]["mi"], RAW_STACK);
    assert_eq!(records[5]["event"], "stop");
    assert_eq!(records[5]["pc"], "0x1000");
    ev.finish(true, serde_json::json!({})).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
