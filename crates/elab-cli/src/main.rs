mod report;
use clap::{Parser, Subcommand};
use elab_core::image::check_image;
use elab_core::{
    run, sha256_hex, Device, ElabError, Evidence, ExperimentSpec, Machine, Result, SerialLink,
};
use serde_json::json;
use std::path::{Path, PathBuf};

const OPS_HELP: &str = r#"EXPERIMENT OPS

  measure        { op: measure, name: <measurement> } # ordered ops declared in measurements
  restore        { op: restore, write: <write-id> }    # exact original bytes + readback
  compare        { op: compare, left: "baseline.value", right: "recovery.value",
                   relation: equal|not_equal|greater, tolerance: 0 }
                 # captures / published fields / attachments; #1, #2 selects repeats

  halt           { op: halt }                     # halt now, without resetting
  resume         { op: resume }                   # continue, do not wait for a stop
  sleep          { op: sleep, ms: 500 }           # host delay; target state unchanged

  watch          { op: watch, expr: "nn_act[4]" }     # one hardware value change; removes watch
  backtrace      { op: backtrace }                   # raw GDB/MI stack frames
  stop           { op: stop, at: <stop>, when: "<cond>", timeout_ms }
  capture        { op: capture, name: <var>, expr: "<expr>" }
  write          { op: write, id: <optional-write-id>, at: "<addr>", kind: f32|f64|i8|u8|u32|i32,
                   value: "<expr>" }            # scalar, or:
                 { op: write, at: "<addr>", kind: u8|u32, values: ["..", ..] }
  write_reg      { op: write_reg, name: "<reg>", value: "<expr>" }
                 # write a CPU register (e.g. a0 at a function entry)
  published      { op: published, stop: <stop>, match: "<expr>", tol, tries,
                   timeout_ms, x, count, channels: [{name, value}, ..] }
  expect         { op: expect, fields: { <channel>: <float>, ... } }
  serial_expect  { op: serial_expect, pattern: "<text>", timeout_ms }
  serial_send    { op: serial_send, text: "<text>" }
  serial_flush   { op: serial_flush }          # discard buffered serial input
  reset          { op: reset, halt: true|false }        # monitor reset / core reset
  power_cycle    { op: power_cycle }                    # profile [power] cycle argv
  attachment     { op: attachment, name: <name>, at: "<addr>", len: <bytes> }
  flash_read     { op: flash_read, name: <name>, offset: "<addr>", len: <bytes> }
                 # raw flash via OpenOCD's flash driver (works even when the
                 # CPU is halted with the cache off)

A phase accepts repeat: <positive count> (default 1). Repeated phases are
labelled name[1], name[2], ...; repeated attachments get unique filenames.

Addresses in stop profiles, [image] verify/model and the ops above accept a
number (0x420094a2) or an ELF symbol expression (&symbol, resolved by GDB).

DEVICE PROFILE (TOML)

  [image]        elf_sha256, verify, [image.model]
                 verify.addr/model.addr may be a symbol expression.
  [published]    x, count
  [[channels]]   name, value
  [power]        cycle = ["argv", "…"]
  [stops]        name = { addr, cond }

TRANSPORT

  transport.gdb    { endpoint, elf, timeout_ms }   # default timeout_ms 10000
  transport.probe  { chip, elf, timeout_ms }   # build with --features probe
  transport.serial { port, baud }
  transport.openocd { config: ["board/x.cfg"], host?, user?,
                      gdb_port: 3333, telnet_port: 4444, start: true|false }
                 # start: true -> elab launches OpenOCD (locally or over ssh
                 # to host) and tunnels the ports; it detects an existing
                 # OpenOCD on the port and reuses it. Errors clearly if a
                 # serial tool (picocom, minicom) holds the UART device.

COMMANDS

  elab run <SPEC> [--runs DIR] [--set NAME=VALUE]     execute phases and save evidence
  elab check <SPEC> [--set NAME=VALUE] validate spec + profile, no hardware
  elab stamp <SPEC>                write the transport ELF hash into the profile
  elab report <RUN_DIR> [--format text|markdown]  render saved observations
  elab verify <RUN_DIR>            re-check recorded expectations
  elab devices <DIR>               list TOML device profiles
  elab ops                         print this reference

ELF paths are relative to the working directory. See README.md and DESIGN.md.
"#;

#[derive(Parser)]
#[command(
    name = "elab",
    version,
    about = "Phased experiments on embedded targets",
    long_about = "elab runs phased experiments against an embedded target.\n\nIt holds one GDB/MI, probe-rs, or serial session, executes YAML phases, checks configured image identity, attempts write restoration on execution errors, and records JSONL evidence.",
    after_help = "ELF paths are relative to the working directory.\n\nExamples:\n  elab devices devices\n  elab check experiments/capture.yaml\n  elab run experiments/capture.yaml\n  elab verify runs/capture-<epoch_ms>\n  elab stamp experiments/capture.yaml\n  elab ops"
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
        /// Override a declared typed parameter (repeatable).
        #[arg(long = "set", value_name = "NAME=VALUE")]
        parameters: Vec<String>,
        /// Path to the experiment YAML.
        #[arg(value_name = "SPEC")]
        spec: PathBuf,
        /// Directory for the evidence directory (default: runs).
        #[arg(long, default_value = "runs", value_name = "DIR")]
        runs: PathBuf,
    },
    /// Render saved observations and comparisons without hardware.
    Report {
        #[arg(value_name = "RUN_DIR")]
        run: PathBuf,
        #[arg(long, default_value = "text", value_parser = ["text", "markdown"])]
        format: String,
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
    /// Validate an experiment + profile without touching hardware.
    #[command(
        long_about = "Load the experiment spec and its device profile, resolve ELF paths and hashes, and report errors. No target is opened.\n\nUseful before hardware is connected, and for iterating on YAML/TOML syntax without a board."
    )]
    Check {
        /// Override a declared typed parameter (repeatable).
        #[arg(long = "set", value_name = "NAME=VALUE")]
        parameters: Vec<String>,
        /// Path to the experiment YAML.
        #[arg(value_name = "SPEC")]
        spec: PathBuf,
    },
    /// Stamp the profile's [image] elf_sha256 from the transport ELF.
    #[command(
        long_about = "Compute the SHA-256 of the experiment's transport ELF and write it into the device profile's [image] elf_sha256.\n\nRun this after a firmware rebuild so the identity check tracks the current build."
    )]
    Stamp {
        /// Path to the experiment YAML.
        #[arg(value_name = "SPEC")]
        spec: PathBuf,
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
        Cmd::Run {
            spec,
            runs,
            parameters,
        } => run_cmd(spec, runs, parameters),
        Cmd::Report { run, format } => report_cmd(run, format == "markdown"),
        Cmd::Verify { run } => verify_cmd(run),
        Cmd::Devices { dir } => devices_cmd(dir),
        Cmd::Check { spec, parameters } => check_cmd(spec, parameters),
        Cmd::Stamp { spec } => stamp_cmd(spec),
        Cmd::Ops => {
            print!("{OPS_HELP}");
            Ok(())
        }
    }
}

fn machine_for(
    spec: &ExperimentSpec,
    device: &Device,
) -> Result<(Box<dyn Machine>, Option<elab_core::openocd::OpenOcdHandle>)> {
    if let Some(p) = &spec.transport.probe {
        #[cfg(feature = "probe")]
        {
            let mut m = elab_probe::ProbeMachine::new(&p.chip, p.timeout_ms)?;
            if let Some(elf) = &p.elf {
                check_image(device, Path::new(elf), &mut m)?;
            }
            return Ok((Box::new(m), None));
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
        // Start or verify OpenOCD first, when the spec asks for one. The
        // handle lives until run_cmd drops it after the run; an existing
        // server is left alone.
        let mut ocd_handle = None;
        let elf = PathBuf::from(&g.elf);
        let mut gdb = elab_gdb::Gdb::new(elf.to_str().unwrap_or(&g.elf))?;
        if let Some(o) = &spec.transport.openocd {
            let h = elab_core::openocd::ensure_running(o)?;
            let (telnet_host, telnet_port) = h.local_telnet();
            gdb.set_openocd_flash(&telnet_host, telnet_port, h.remote_target());
            let ep = h.local_gdb_endpoint();
            ocd_handle = Some(h);
            gdb.connect(&ep)?;
        } else {
            gdb.connect(&g.endpoint)?;
        }
        check_image(device, &elf, &mut gdb)?;
        return Ok((Box::new(gdb), ocd_handle));
    }
    Ok((Box::new(elab_core::NoMachine), None))
}

fn run_cmd(spec_path: PathBuf, runs: PathBuf, parameters: Vec<String>) -> Result<()> {
    let spec = ExperimentSpec::load_with_parameters(&spec_path, &parameters)?;
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

    let (mut machine, _ocd) = machine_for(&spec, &device)?;

    let mut ev = Evidence::create(&runs, &spec.name)?;
    ev.event(json!({"event":"parameters", "values":spec.resolved_parameters}))?;
    let result = run(
        &spec,
        &device,
        machine.as_mut(),
        serial.as_mut().map(|s| s as &mut dyn SerialLink),
        &mut ev,
    );
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            // Detach after recording the failure so the error is reported
            // even if the transport is wedged.
            let _ = machine.detach();
            if let Some(mut s) = serial {
                s.close();
            }
            let _ = ev.finish(false, json!({"error": e.to_string()}));
            return Err(e);
        }
    };
    let _ = machine.detach();
    if let Some(mut s) = serial {
        s.close();
    }
    let recorded = report::events(&std::fs::read_to_string(ev.dir().join("events.jsonl"))?)?;
    if recorded.iter().any(|e| e["event"] == "compare") {
        print!("{}", report::render(&recorded, false)?);
    } else {
        print_table(&result);
    }

    let mut manifest = json!({
        "name": spec.name,
        "parameters": spec.resolved_parameters,
        "device": device.name,
        "spec": spec_path.display().to_string(),
        "spec_sha256": sha256_hex(&std::fs::read(&spec_path)?),
        "device_profile": device_path.display().to_string(),
        "device_sha256": sha256_hex(&std::fs::read(&device_path)?),
        "elab_version": env!("CARGO_PKG_VERSION"),
        "captures": result.captures,
        "rows": result.rows,
        "failures": result.failures,
        "attachments": result.attachments,
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
    if !r.captures.is_empty() {
        println!("{:<10} {:<24} value", "phase", "capture");
        for c in &r.captures {
            println!("{:<10} {:<24} {}", c.phase, c.name, c.value);
        }
    }
    if !r.rows.is_empty() {
        if !r.captures.is_empty() {
            println!();
        }
        print!("{:<10} {:>13}", "phase", "sample");
        for c in &r.channels {
            print!(" {:>12}", c);
        }
        println!(" {:>8}", "count");
    }
    for row in &r.rows {
        print!("{:<10} {:>13.9}", row.phase, row.x);
        for c in &r.channels {
            let v = row.values.get(c).copied().unwrap_or(f64::NAN);
            print!(" {:>12.9}", v);
        }
        println!(" {:>8}", row.count);
    }
    for a in &r.attachments {
        match &a.hex {
            Some(h) => println!("{:<10} @{} len={} {}", a.phase, a.addr, a.len, hex_short(h)),
            None => println!(
                "{:<10} @{} len={} sha256={}",
                a.phase,
                a.addr,
                a.len,
                &a.sha256[..16]
            ),
        }
    }
}

/// Short hex string for the table (elide the middle of long runs).
fn hex_short(h: &str) -> String {
    if h.len() <= 32 {
        h.to_string()
    } else {
        format!("{}…{}", &h[..16], &h[h.len() - 16..])
    }
}

fn verify_cmd(run: PathBuf) -> Result<()> {
    let manifest_text = std::fs::read_to_string(run.join("manifest.json"))?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest_text)?;
    let events_text = std::fs::read_to_string(run.join("events.jsonl"))?;
    let records = report::events(&events_text)?;
    report::verify_comparisons(&records)?;
    let comparisons = records.iter().filter(|e| e["event"] == "compare").count();
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
        "manifest ok={:?} expects={} comparisons={comparisons} failed={}",
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

fn report_cmd(run: PathBuf, markdown: bool) -> Result<()> {
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run.join("manifest.json"))?)?;
    println!(
        "Recorded run status: {}\n",
        if manifest["ok"] == true {
            "completed"
        } else {
            "failed"
        }
    );
    print!(
        "{}",
        report::render(
            &report::events(&std::fs::read_to_string(run.join("events.jsonl"))?)?,
            markdown
        )?
    );
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
        println!("{:<20} {}", d.name, p.display());
    }
    Ok(())
}

fn check_cmd(spec_path: PathBuf, parameters: Vec<String>) -> Result<()> {
    let spec = ExperimentSpec::load_with_parameters(&spec_path, &parameters)?;
    let base = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let device_path = base.join(&spec.device);
    let device = Device::load(&device_path)?;
    // ELF reference check: if the transport supplies one, it must exist.
    let elf = match &spec.transport.gdb {
        Some(g) => Some(g.elf.clone()),
        None => spec.transport.probe.as_ref().and_then(|p| p.elf.clone()),
    };
    if let Some(elf) = &elf {
        let elf_path = PathBuf::from(&elf);
        if !elf_path.exists() {
            return Err(ElabError::msg(format!("ELF path '{}' does not exist", elf)));
        }
    }
    println!(
        "spec ok: {} phases, device profile '{}' ok, ELF {:?}",
        spec.phases.len(),
        device.name,
        elf
    );
    Ok(())
}

fn stamp_cmd(spec_path: PathBuf) -> Result<()> {
    let spec = ExperimentSpec::load(&spec_path)?;
    let base = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let device_path = base.join(&spec.device);
    let elf = match &spec.transport.gdb {
        Some(g) => g.elf.clone(),
        None => match spec.transport.probe.as_ref().and_then(|p| p.elf.clone()) {
            Some(e) => e,
            None => return Err(ElabError::msg("no transport ELF to stamp from")),
        },
    };
    let elf_bytes = std::fs::read(&elf)?;
    let hash = sha256_hex(&elf_bytes);
    let mut device = Device::load(&device_path)?;
    device.image.elf_sha256 = Some(hash.clone());
    let out = toml::to_string_pretty(&device)?;
    std::fs::write(&device_path, out)?;
    println!("stamped {} into {}", hash, device_path.display());
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
