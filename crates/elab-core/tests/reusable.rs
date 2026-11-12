use elab_core::{run, Device, Evidence, ExperimentSpec, Machine, Result, Value};

#[derive(Default)]
struct Target {
    bytes: Vec<u8>,
    stops: Vec<u64>,
    corrupt_restore: bool,
    writes: usize,
}
impl Machine for Target {
    fn stop(&mut self, addr: u64, _: Option<&str>) -> Result<u64> {
        self.stops.push(addr);
        Ok(addr)
    }
    fn evaluate(&mut self, expr: &str) -> Result<Value> {
        if expr == "byte" {
            return Ok(Value::UInt(self.bytes[0] as u64));
        }
        Ok(Value::from_gdb(expr))
    }
    fn read_memory(&mut self, _: u64, len: usize) -> Result<Vec<u8>> {
        Ok(self.bytes[..len].to_vec())
    }
    fn write_memory(&mut self, _: u64, bytes: &[u8]) -> Result<()> {
        self.writes += 1;
        self.bytes = bytes.to_vec();
        if self.corrupt_restore && self.writes > 1 {
            self.bytes[0] = 99;
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
fn execute(yaml: &str, target: &mut Target) -> Result<elab_core::RunResult> {
    let spec: ExperimentSpec = serde_yaml::from_str(yaml)?;
    let device: Device = toml::from_str(
        "name = 'mock'\n[image]\n[stops]\none = {addr = 4096}\ntwo = {addr = 8192}\n",
    )?;
    let base = std::env::temp_dir().join(format!(
        "elab-reusable-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut ev = Evidence::create(&base, "test")?;
    let result = run(&spec, &device, target, None, &mut ev);
    drop(ev);
    std::fs::remove_dir_all(base).unwrap();
    result
}
const SPEC: &str = r#"
name: reusable
device: mock.toml
transport: {}
measurements:
  sample:
    - {op: stop, at: one}
    - {op: capture, name: byte, expr: byte}
    - {op: stop, at: two}
    - {op: attachment, name: vector, at: '4096', len: 2}
phases:
  - name: baseline
    ops: [{op: measure, name: sample}]
  - name: mutation
    ops:
      - {op: write, id: edit, at: '4096', kind: u8, values: ['0', '255']}
      - {op: measure, name: sample}
      - {op: compare, left: mutation.byte, right: baseline.byte, relation: not_equal}
  - name: recovery
    ops:
      - {op: restore, write: edit}
      - {op: measure, name: sample}
      - {op: compare, left: recovery.vector, right: baseline.vector}
"#;
#[test]
fn reused_measurements_keep_order_and_restore_exact_bytes() {
    let mut t = Target {
        bytes: vec![127, 128],
        ..Default::default()
    };
    let result = execute(SPEC, &mut t).expect("reusable experiment should run");
    assert!(result.ok);
    assert_eq!(t.bytes, [127, 128]);
    assert_eq!(t.stops, [4096, 8192, 4096, 8192, 4096, 8192]);
    assert_eq!(result.captures.len(), 3);
}
#[test]
fn restoration_readback_failure_is_visible() {
    let mut t = Target {
        bytes: vec![127, 128],
        corrupt_restore: true,
        ..Default::default()
    };
    let error = execute(SPEC, &mut t).unwrap_err().to_string();
    assert!(
        error.contains("restore") && error.contains("readback"),
        "{error}"
    );
}
#[test]
fn comparison_failure_does_not_skip_recovery() {
    let mut t = Target {
        bytes: vec![127, 128],
        ..Default::default()
    };
    let result = execute(
        &SPEC.replace("relation: not_equal", "relation: equal"),
        &mut t,
    )
    .unwrap();
    assert!(!result.ok);
    assert_eq!(t.bytes, [127, 128]);
}
#[test]
fn recursive_measurements_fail_before_touching_target() {
    let yaml = "name: x\ndevice: mock.toml\ntransport: {}\nmeasurements:\n  loop: [{op: measure, name: loop}]\nphases:\n  - name: x\n    ops: [{op: measure, name: loop}]\n";
    let mut t = Target::default();
    let error = execute(yaml, &mut t).unwrap_err().to_string();
    assert!(error.contains("recursive"), "{error}");
    assert!(t.stops.is_empty());
}

#[test]
fn repeated_named_writes_can_be_restored_each_iteration() {
    let yaml="name: repeat\ndevice: mock.toml\ntransport: {}\nphases:\n  - name: sample\n    repeat: 3\n    ops:\n      - {op: write, id: edit, at: '4096', kind: u8, values: ['1','2']}\n      - {op: restore, write: edit}\n";
    let mut t = Target {
        bytes: vec![127, 128],
        ..Default::default()
    };
    assert!(execute(yaml, &mut t).unwrap().ok);
    assert_eq!(t.bytes, [127, 128]);
    assert_eq!(t.writes, 6);
}
#[test]
fn missing_or_ambiguous_observations_fail_without_skipping_restore() {
    for reference in ["baseline.missing", "baseline.byte"] {
        let yaml = SPEC
            .replace(
                "ops: [{op: measure, name: sample}]",
                "ops: [{op: measure, name: sample}, {op: measure, name: sample}]",
            )
            .replace("right: baseline.byte", &format!("right: {reference}"));
        let mut t = Target {
            bytes: vec![127, 128],
            ..Default::default()
        };
        let result = execute(&yaml, &mut t).unwrap();
        assert!(!result.ok);
        assert_eq!(t.bytes, [127, 128]);
    }
}
#[test]
fn comparison_tolerances_and_large_integers() {
    use elab_core::{
        observation::{compare, Observation},
        spec::Relation,
    };
    let float = |f| Observation::Scalar(Value::Float(f));
    assert!(compare(&float(1.01), &float(1.0), Relation::Equal, 0.02).unwrap());
    assert!(!compare(&float(1.01), &float(1.0), Relation::NotEqual, 0.02).unwrap());
    assert!(compare(&float(f64::NAN), &float(1.0), Relation::NotEqual, 0.0).is_err());
    let a = Observation::Scalar(Value::UInt(u64::MAX));
    let b = Observation::Scalar(Value::UInt(u64::MAX - 1));
    assert!(!compare(&a, &b, Relation::Equal, 0.0).unwrap());
    assert!(compare(&a, &b, Relation::Greater, 0.0).unwrap());
}
#[test]
fn parameters_resolve_expressions_without_yaml_injection() {
    let yaml="name: params\ndevice: mock.toml\ntransport: {}\nparameters:\n  address: {kind: address, default: '0x1000'}\n  value: {kind: u8, default: 1}\nphases:\n  - name: mutation\n    ops:\n      - {op: write, id: edit, at: '${address}', kind: u8, value: '${value}'}\n      - {op: restore, write: edit}\n";
    let raw: ExperimentSpec = serde_yaml::from_str(yaml).unwrap();
    let spec = raw.prepare(&["value=255".into()]).unwrap();
    assert_eq!(spec.resolved_parameters["value"], "255");
    assert!(raw.prepare(&["value=0; call reset()".into()]).is_err());
    assert!(raw.prepare(&["address=-1".into()]).is_err());
}

#[test]
fn overlapping_writes_require_reverse_restoration() {
    for (restores, valid) in [
        ("      - {op: restore, write: first}\n", false),
        (
            "      - {op: restore, write: second}\n      - {op: restore, write: first}\n",
            true,
        ),
    ] {
        let yaml=format!("name: overlap\ndevice: mock.toml\ntransport: {{}}\nphases:\n  - name: mutation\n    ops:\n      - {{op: write, id: first, at: '4096', kind: u8, values: ['1','2']}}\n      - {{op: write, id: second, at: '4096', kind: u8, values: ['3','4']}}\n{restores}");
        let mut t = Target {
            bytes: vec![127, 128],
            ..Default::default()
        };
        let result = execute(&yaml, &mut t);
        assert_eq!(result.is_ok(), valid);
        assert_eq!(t.bytes, [127, 128]);
        if !valid {
            assert!(result.unwrap_err().to_string().contains("reverse order"));
        }
    }
}
#[test]
fn later_error_does_not_replay_an_already_restored_write() {
    let yaml="name: error\ndevice: mock.toml\ntransport: {}\nphases:\n  - name: mutation\n    ops:\n      - {op: write, id: edit, at: '4096', kind: u8, values: ['1','2']}\n      - {op: restore, write: edit}\n      - {op: restore, write: unknown}\n";
    let mut t = Target {
        bytes: vec![127, 128],
        ..Default::default()
    };
    assert!(execute(yaml, &mut t).is_err());
    assert_eq!(t.bytes, [127, 128]);
    assert_eq!(t.writes, 2);
}
