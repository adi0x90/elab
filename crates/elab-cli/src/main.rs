use clap::{Parser, Subcommand};
use elab_core::image::check_image;
use elab_core::{
    run, sha256_hex, Device, ElabError, Evidence, ExperimentSpec, Machine, Result, SerialLink,
};
use serde_json::json;
use std::path::{Path, PathBuf};

const OPS_HELP: &str = r#"EXPERIMENT OPS

  stop           { op: stop, at: <stop>, when: "<cond>" }
  capture        { op: capture, name: <var>, expr: "<expr>" }
  write          { op: write, at: "<addr>", kind: f32|f64|i8|u8|u32|i32, value: "<expr>" }
  published      { op: published, stop: <stop>, match: "<expr>", tol, tries }
  expect         { op: expect, fields: { <channel>: <float>, ... } }
  serial_expect  { op: serial_expect, pattern: "<text>", timeout_ms }
  serial_send    { op: serial_send, text: "<text>" }
  reset          { op: reset, halt: true|false }        # monitor reset / core reset
  power_cycle    { op: power_cycle }                    # profile [power] cycle argv
  attachment     { op: attachment, name: <name>, at: "<addr>", len: <bytes> }

DEVICE PROFILE (TOML)

  [image]        elf_sha256, verify, [image.model]
  [published]    x, count
  [[channels]]   name, value
  [power]        cycle = ["argv", "…"]
  [stops]        name = { addr, cond }

TRANSPORT

  transport.gdb    { endpoint, elf }
  transport.probe  { chip, elf, timeout_ms }   # build with --features probe
  transport.serial { port, baud }

ELF paths are relative to the working directory. See README.md and DESIGN.md.
"#;

#[derive(Parser)]
#[command(
    name = "elab",
    version,
    about = "Phased experiments on embedded targets",
    long_about = "elab runs phased experiments against an embedded target.\n\nIt holds one GDB/MI, probe-rs, or serial session, executes YAML phases, checks configured image identity, attempts write restoration on execution errors, and records JSONL evidence.",
    after_help = "ELF paths are relative to the working directory.\n\nExamples:\n  elab devices devices\n  elab run experiments/capture.yaml\n  elab verify runs/capture-<epoch_ms>\n  elab ops"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run an experiment spec against a device profile.
    #[command(
        long_about = "Run every phase in a YAML experiment against the target.\n\nAttaches GDB or probe-rs, checks the ELF, live-code and model hashes, runs the ops, prints the phase table, and writes runs/<name>-<epoch_ms>/ with events.jsonl and manifest.json.",
        after_help = "Example:\n  elab run experiments/capture.yaml --runs ./runs"
    )]
    Run {
        /// Path to the experiment YAML.
        #[arg(value_name = "SPEC")]
        spec: PathBuf,
        /// Directory for the evidence directory (default: runs).
        #[arg(long, default_value = "runs", value_name = "DIR")]
        runs: PathBuf,
    },
    /// Re-check a recorded run without hardware.
    Verify {
        /// Path to a run directory created by `elab run`.
        #[arg(value_name = "RUN_DIR")]
        run: PathBuf,
    },
    /// List device profiles in a directory.
    Devices {
        /// Directory containing .toml device profiles.
        #[arg(value_name = "DIR")]
        dir: PathBuf,
    },
    /// Print the experiment op and profile reference.
    Ops,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = dispatch(cli) {
        eprintln!("elab: {e}");
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Run { spec, runs } => run_cmd(spec, runs),
        Cmd::Verify { run } => verify_cmd(run),
        Cmd::Devices { dir } => devices_cmd(dir),
        Cmd::Ops => {
            print!("{OPS_HELP}");
            Ok(())
        }
    }
}

fn machine_for(spec: &ExperimentSpec, device: &Device) -> Result<Box<dyn Machine>> {
    if let Some(p) = &spec.transport.probe {
        #[cfg(feature = "probe")]
        {
            let mut m = elab_probe::ProbeMachine::new(&p.chip, p.timeout_ms)?;
            if let Some(elf) = &p.elf {
                check_image(device, Path::new(elf), &mut m)?;
            }
            return Ok(Box::new(m));
        }
        #[cfg(not(feature = "probe"))]
        {
            let _ = (p, device);
            return Err(ElabError::msg(
                "this build has no probe support; rebuild with --features probe",
            ));
        }
    }
    if let Some(g) = &spec.transport.gdb {
        let elf = PathBuf::from(&g.elf);
        let mut gdb = elab_gdb::Gdb::new(elf.to_str().unwrap_or(&g.elf))?;
        gdb.connect(&g.endpoint)?;
        check_image(device, &elf, &mut gdb)?;
        return Ok(Box::new(gdb));
    }
    Ok(Box::new(elab_core::NoMachine))
}

fn run_cmd(spec_path: PathBuf, runs: PathBuf) -> Result<()> {
    let spec = ExperimentSpec::load(&spec_path)?;
    let base = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let device_path = base.join(&spec.device);
    let device = Device::load(&device_path)?;

    // Serial first: opening the C6 console can reset the chip. Let it boot
    // before GDB attaches, otherwise a breakpoint insert hits the bootloader.
    let mut serial = match &spec.transport.serial {
        Some(s) => Some(elab_serial::Serial::open(&s.port, s.baud)?),
        None => None,
    };
    if serial.is_some() {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }

    let mut machine = machine_for(&spec, &device)?;

    let mut ev = Evidence::create(&runs, &spec.name)?;
    let result = run(
        &spec,
        &device,
        machine.as_mut(),
        serial.as_mut().map(|s| s as &mut dyn SerialLink),
        &mut ev,
    );
    let _ = machine.detach();
    if let Some(mut s) = serial {
        s.close();
    }
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            let _ = ev.finish(false, json!({"error": e.to_string()}));
            return Err(e);
        }
    };
    print_table(&result);

    let mut manifest = json!({
        "name": spec.name,
        "device": device.name,
        "spec": spec_path.display().to_string(),
        "spec_sha256": sha256_hex(&std::fs::read(&spec_path)?),
        "device_profile": device_path.display().to_string(),
        "device_sha256": sha256_hex(&std::fs::read(&device_path)?),
        "elab_version": env!("CARGO_PKG_VERSION"),
        "rows": result.rows,
        "failures": result.failures,
    });
    if let Some(g) = &spec.transport.gdb {
        manifest["elf"] = json!(g.elf);
        manifest["elf_sha256"] = json!(sha256_hex(&std::fs::read(&g.elf)?));
    }
    if let Some(p) = &spec.transport.probe {
        if let Some(elf) = &p.elf {
            manifest["elf"] = json!(elf);
            manifest["elf_sha256"] = json!(sha256_hex(&std::fs::read(elf)?));
        }
    }

    let dir = ev.finish(result.ok, manifest)?;
    println!("evidence {}", dir.display());
    if !result.ok {
        return Err(ElabError::msg("expectations failed"));
    }
    Ok(())
}

fn print_table(r: &elab_core::RunResult) {
    print!("{:<10} {:>13}", "phase", "sample");
    for c in &r.channels {
        print!(" {:>12}", c);
    }
    println!(" {:>8}", "count");
    for row in &r.rows {
        print!("{:<10} {:>13.9}", row.phase, row.x);
        for c in &r.channels {
            let v = row.values.get(c).copied().unwrap_or(f64::NAN);
            print!(" {:>12.9}", v);
        }
        println!(" {:>8}", row.count);
    }
}

fn verify_cmd(run: PathBuf) -> Result<()> {
    let manifest_text = std::fs::read_to_string(run.join("manifest.json"))?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_text)?;
    let events_text = std::fs::read_to_string(run.join("events.jsonl"))?;
    let mut expects = 0usize;
    let mut bad = 0usize;
    let mut events = 0u64;
    for line in events_text.lines() {
        let v: serde_json::Value = serde_json::from_str(line)?;
        events += 1;
        if v.get("event").and_then(|e| e.as_str()) == Some("expect") {
            expects += 1;
            let matches = v
                .get("got")
                .and_then(|x| x.as_f64())
                .zip(v.get("want").and_then(|x| x.as_f64()))
                .is_some_and(|(got, want)| (got - want).abs() <= 1e-6);
            if !matches || v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
                bad += 1;
                println!("FAIL {line}");
            }
        }
    }
    println!(
        "manifest ok={:?} expects={} failed={}",
        manifest.get("ok"),
        expects,
        bad
    );
    if bad > 0
        || manifest.get("ok").and_then(|o| o.as_bool()) != Some(true)
        || manifest.get("events").and_then(|n| n.as_u64()) != Some(events)
    {
        return Err(ElabError::msg("verification failed"));
    }
    Ok(())
}

fn devices_cmd(dir: PathBuf) -> Result<()> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
        .collect();
    paths.sort();
    for p in paths {
        let d = Device::load(&p)?;
        println!("{:<12} {}  ({})", d.name, d.description, p.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_rechecks_values_and_rejects_incomplete_evidence() {
        let dir = std::env::temp_dir().join(format!("elab-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cases = [
            (
                json!({"ok": true, "events": 1}),
                json!({"event": "expect", "want": 1.0, "got": 1.0, "ok": true}),
                true,
            ),
            (
                json!({"ok": true, "events": 1}),
                json!({"event": "expect", "want": 1.0, "got": 9.0, "ok": true}),
                false,
            ),
            (
                json!({"ok": true, "events": 1}),
                json!({"event": "expect", "want": 1.0, "got": 1.0}),
                false,
            ),
            (
                json!({"ok": true, "events": 2}),
                json!({"event": "expect", "want": 1.0, "got": 1.0, "ok": true}),
                false,
            ),
            (
                json!({"ok": false, "events": 1}),
                json!({"event": "expect", "want": 1.0, "got": 1.0, "ok": true}),
                false,
            ),
        ];
        for (manifest, event, valid) in cases {
            std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
            std::fs::write(dir.join("events.jsonl"), format!("{event}\n")).unwrap();
            assert_eq!(
                verify_cmd(dir.clone()).is_ok(),
                valid,
                "{manifest}: {event}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
