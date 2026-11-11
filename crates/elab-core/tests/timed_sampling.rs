use elab_core::{run, Device, Evidence, ExperimentSpec, Machine, Result, Value};
use std::time::{Duration, Instant};

struct Target {
    running: bool,
    samples: u64,
    calls: Vec<&'static str>,
    resumed_at: Option<Instant>,
    fail_read: bool,
}
impl Machine for Target {
    fn halt(&mut self) -> Result<()> {
        if let Some(at) = self.resumed_at {
            assert!(at.elapsed() >= Duration::from_millis(20));
        }
        self.running = false;
        self.samples += 1;
        self.calls.push("halt");
        Ok(())
    }
    fn resume(&mut self) -> Result<()> {
        self.running = true;
        self.resumed_at = Some(Instant::now());
        self.calls.push("resume");
        Ok(())
    }
    fn stop(&mut self, _: u64, _: Option<&str>) -> Result<u64> {
        unreachable!()
    }
    fn evaluate(&mut self, _: &str) -> Result<Value> {
        assert!(!self.running, "captures must follow a synchronized halt");
        self.calls.push("capture");
        Ok(Value::UInt(self.samples))
    }
    fn read_memory(&mut self, _: u64, _: usize) -> Result<Vec<u8>> {
        assert!(!self.running);
        if self.fail_read {
            return Err(elab_core::ElabError::msg("read failed"));
        }
        Ok(vec![self.samples as u8])
    }
    fn write_memory(&mut self, _: u64, _: &[u8]) -> Result<()> {
        unreachable!()
    }
    fn reset(&mut self, _: bool) -> Result<()> {
        unreachable!()
    }
    fn detach(&mut self) -> Result<()> {
        Ok(())
    }
}
fn target() -> Target {
    Target {
        running: true,
        samples: 0,
        calls: vec![],
        resumed_at: None,
        fail_read: false,
    }
}
fn spec() -> ExperimentSpec {
    serde_yaml::from_str(
        r#"
name: sampling
device: unused.toml
transport: {}
phases:
  - name: sample
    repeat: 3
    ops:
      - { op: halt }
      - { op: capture, name: word, expr: word }
      - { op: attachment, name: words, at: '0x1000', len: 1 }
      - { op: resume }
      - { op: sleep, ms: 20 }
"#,
    )
    .unwrap()
}
#[test]
fn timed_sampling_preserves_each_iteration_and_runs_between_halts() {
    let spec = spec();
    let device: Device = toml::from_str("name='test'\n[image]\n[stops]\n").unwrap();
    let root = std::env::temp_dir().join(format!("elab-timed-{}", std::process::id()));
    let mut ev = Evidence::create(&root, "samples").unwrap();
    let mut target = target();
    let start = Instant::now();
    let result = run(&spec, &device, &mut target, None, &mut ev).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(60));
    assert!(target.running);
    assert_eq!(target.calls, ["halt", "capture", "resume"].repeat(3));
    assert_eq!(
        result
            .captures
            .iter()
            .map(|c| c.phase.as_str())
            .collect::<Vec<_>>(),
        ["sample[1]", "sample[2]", "sample[3]"]
    );
    assert_eq!(
        result
            .captures
            .iter()
            .map(|c| c.value.as_str())
            .collect::<Vec<_>>(),
        ["1", "2", "3"]
    );
    let attachments: Vec<_> = ev
        .events
        .iter()
        .filter(|e| e["event"] == "attachment")
        .collect();
    assert_eq!(attachments.len(), 3);
    for (i, e) in attachments.iter().enumerate() {
        let bytes = std::fs::read(e["path"].as_str().unwrap()).unwrap();
        assert_eq!(bytes, vec![(i + 1) as u8]);
        assert_eq!(e["sha256"], elab_core::sha256_hex(&bytes));
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn zero_repetitions_are_rejected() {
    let yaml = "name: invalid\ndevice: unused\ntransport: {}\nphases:\n - name: empty\n   repeat: 0\n   ops: []\n";
    assert!(serde_yaml::from_str::<ExperimentSpec>(yaml).is_err());
}

#[test]
fn failed_capture_releases_an_explicit_halt() {
    let spec = spec();
    let device: Device = toml::from_str("name='test'\n[image]\n[stops]\n").unwrap();
    let root = std::env::temp_dir().join(format!("elab-timed-error-{}", std::process::id()));
    let mut ev = Evidence::create(&root, "error").unwrap();
    let mut target = target();
    target.fail_read = true;
    let err = run(&spec, &device, &mut target, None, &mut ev).unwrap_err();
    assert!(err.to_string().contains("read failed"));
    assert!(target.running);
    assert_eq!(target.calls, ["halt", "capture", "resume"]);
    std::fs::remove_dir_all(root).unwrap();
}
