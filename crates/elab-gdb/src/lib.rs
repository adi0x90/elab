//! GDB/MI transport for elab.
//!
//! GDB stays the expression and breakpoint engine. This module starts one MI
//! session, inserts hardware temporary breakpoints, waits for `*stopped`, and
//! evaluates GDB expressions (for example `*(float*)($sp+12)`).

use elab_core::{ElabError, Machine, Result, Value, WatchHit};
use regex::Regex;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

pub struct Gdb {
    child: Child,
    stdin: ChildStdin,
    /// Lines arrive from a dedicated reader thread; `line()` bounds its wait
    /// with the configured timeout so a hung target cannot hang the run.
    lines: Receiver<String>,
    timeout_ms: u64,
    /// Optional OpenOCD telnet endpoint (host, port) for flash-driver
    /// commands; set by the CLI when the spec configures openocd.
    openocd_telnet: Option<(String, u16)>,
    /// user@host to fetch OpenOCD-written files (flash dumps) from; None
    /// when OpenOCD runs on this machine.
    openocd_scp_target: Option<String>,
}

/// Drain `stdout` on a thread so the main thread can time out waiting for a
/// line. The thread holds the reader for the process lifetime.
fn start_reader(stdout: ChildStdout) -> Receiver<String> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut s = String::new();
        loop {
            s.clear();
            match reader.read_line(&mut s) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(s.trim_end().to_string()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

struct Record {
    kind: char,
    fields: Vec<(String, String)>,
    raw: String,
}

impl Record {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

impl Gdb {
    pub fn new(elf: &str) -> Result<Self> {
        let gdb = std::env::var("ELAB_GDB").unwrap_or_else(|_| "gdb-multiarch".to_string());
        let mut child = Command::new(gdb)
            .args(["--interpreter=mi2", "-q", elf])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| ElabError::Machine(format!("cannot start gdb: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ElabError::Machine("no gdb stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ElabError::Machine("no gdb stdout".into()))?;
        let mut gdb = Self {
            child,
            stdin,
            lines: start_reader(stdout),
            timeout_ms: 10_000,
            openocd_telnet: None,
            openocd_scp_target: None,
        };
        gdb.cmd("-gdb-set pagination off")?;
        gdb.cmd("-gdb-set confirm off")?;
        gdb.cmd("-gdb-set remotetimeout 10")?;
        Ok(gdb)
    }

    pub fn connect(&mut self, endpoint: &str) -> Result<()> {
        self.cmd(&format!("-target-select extended-remote {endpoint}"))?;
        Ok(())
    }

    /// Attach the OpenOCD telnet endpoint used for flash-driver commands.
    pub fn set_openocd_flash(&mut self, host: &str, port: u16, scp_target: Option<&str>) {
        self.openocd_telnet = Some((host.to_string(), port));
        self.openocd_scp_target = scp_target.map(|s| s.to_string());
    }

    /// Read raw flash through OpenOCD's flash driver via `monitor flash
    /// read_bank`. OpenOCD writes a temp file on its own host and elab reads
    /// it back, so this works even when the CPU is halted with the cache off
    /// (a plain memory read then returns unmapped bytes). Requires the telnet
    /// port of the same OpenOCD for cleanup of the temp file.
    fn flash_read_via_monitor(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        use elab_core::openocd::Telnet;
        let host = self
            .openocd_telnet
            .as_ref()
            .map(|(h, _)| h.clone())
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let port = self
            .openocd_telnet
            .as_ref()
            .map(|(_, p)| *p)
            .unwrap_or(4444);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let tmp = format!("/tmp/elab-flash-{ts}.bin");
        let mut t = Telnet::connect(&host, port, 5000)?;
        // Bank 0 = main flash on all supported targets.
        let out = t.cmd(
            &format!("flash read_bank 0 {tmp} 0x{offset:x} 0x{len:x}"),
            60_000,
        )?;
        if out.contains("error") || out.contains("Error") {
            let _ = t.cmd(&format!("exec rm -f {tmp}"), 5000);
            return Err(ElabError::Machine(format!(
                "flash read_bank failed: {}",
                out.trim()
            )));
        }
        // Read the file from wherever OpenOCD ran, then clean it up. With a
        // tunnel, the telnet endpoint is always 127.0.0.1 — the scp target
        // is what tells us the file lives on a remote host.
        let bytes = if let Some(target) = &self.openocd_scp_target {
            // Remote OpenOCD: scp the file back, read, remove.
            let local = std::env::temp_dir().join(format!("elab-flash-{ts}.bin"));
            let st = Command::new("scp")
                .arg("-q")
                .arg(format!("{target}:{tmp}"))
                .arg(&local)
                .status()
                .map_err(|e| ElabError::Machine(format!("scp flash dump: {e}")))?;
            if !st.success() {
                return Err(ElabError::Machine(format!(
                    "scp flash dump {target}:{tmp} failed"
                )));
            }
            let b = std::fs::read(&local).map_err(|e| {
                ElabError::Machine(format!("flash dump file {tmp} unreadable: {e}"))
            })?;
            let _ = std::fs::remove_file(&local);
            b
        } else {
            std::fs::read(&tmp)
                .map_err(|e| ElabError::Machine(format!("flash dump file {tmp} unreadable: {e}")))?
        };
        let _ = t.cmd(&format!("exec rm -f {tmp}"), 5000);
        if bytes.len() != len {
            return Err(ElabError::Machine(format!(
                "flash read_bank returned {} bytes, expected {len}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    pub fn stop_at(&mut self, addr: u64, cond: Option<&str>) -> Result<u64> {
        // Halt first: OpenOCD may need the core stopped to access flash when
        // it inserts the breakpoint, especially right after a serial reset.
        let _ = self.cmd("-interpreter-exec console \"monitor halt\"");
        let mut c = String::from("-break-insert -h -t");
        if let Some(cond) = cond {
            c.push_str(&format!(" -c \"{}\"", escape(cond)));
        }
        c.push_str(&format!(" *0x{addr:x}"));
        self.cmd(&c)?;
        self.resume()?;
        let pc = self.evaluate("$pc")?.as_u64()?;
        if pc != addr {
            return Err(ElabError::Machine(format!(
                "stopped at 0x{pc:x}, expected 0x{addr:x}"
            )));
        }
        Ok(pc)
    }

    fn resume(&mut self) -> Result<Record> {
        self.send("-exec-continue")?;
        loop {
            let line = self.line()?;
            if line.starts_with("^error") {
                let r = parse(&line);
                return Err(ElabError::Machine(
                    r.get("msg").unwrap_or("continue failed").to_string(),
                ));
            }
            if line.starts_with("*stopped") {
                return Ok(parse(&line));
            }
            // ^running, *running, =thread-*, ~"...", &"..." and so on are skipped
        }
    }

    /// One-shot write watch. Do not mistake an unrelated stop for a hit.
    pub fn watch(&mut self, expr: &str) -> Result<WatchHit> {
        self.cmd("-interpreter-exec console \"monitor halt\"")?;
        let created = self.cmd(&format!("-break-watch \"{}\"", escape(expr)))?;
        let number = nested(created.get("wpt"), "number")
            .ok_or_else(|| ElabError::Machine(format!("no watchpoint number: {}", created.raw)))?;
        if !number.bytes().all(|b| b.is_ascii_digit()) || number.is_empty() {
            return Err(ElabError::Machine("invalid watchpoint number".into()));
        }
        let result = (|| {
            let info = self.cmd(&format!("-break-info {number}"))?;
            if !info.raw.contains("type=\"hw watchpoint\"") {
                return Err(ElabError::Machine(format!(
                    "hardware watchpoint required: {}",
                    info.raw
                )));
            }
            let stopped = self.resume()?;
            watch_hit(&stopped, &number)
        })();
        // Delete only our own watch, including when continue fails or stops elsewhere.
        let cleanup = self.cmd(&format!("-break-delete {number}"));
        match (result, cleanup) {
            (Ok(hit), Ok(_)) => Ok(hit),
            (Err(e), Ok(_)) => Err(e),
            (result, Err(e)) => Err(ElabError::Machine(format!(
                "watch result {result:?}; could not delete watchpoint {number}: {e}"
            ))),
        }
    }

    pub fn backtrace(&mut self) -> Result<String> {
        Ok(self.cmd("-stack-list-frames")?.raw)
    }

    pub fn evaluate(&mut self, expr: &str) -> Result<Value> {
        let r = self.cmd(&format!("-data-evaluate-expression \"{}\"", escape(expr)))?;
        let raw = r
            .get("value")
            .ok_or_else(|| ElabError::Machine(format!("no value for {expr}")))?;
        Ok(Value::from_gdb(raw))
    }

    pub fn read_memory(&mut self, addr: u64, len: usize) -> Result<Vec<u8>> {
        let r = self.cmd(&format!("-data-read-memory-bytes 0x{addr:x} {len}"))?;
        let re = Regex::new(r#"contents="([0-9a-fA-F]*)""#).unwrap();
        let caps = re
            .captures(&r.raw)
            .ok_or_else(|| ElabError::Machine(format!("no memory contents at 0x{addr:x}")))?;
        let hex = &caps[1];
        hex::decode(hex).map_err(|e| ElabError::Machine(format!("bad memory hex: {e}")))
    }

    pub fn write_memory(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        self.cmd(&format!(
            "-data-write-memory-bytes 0x{addr:x} \"{}\"",
            hex::encode(bytes)
        ))?;
        Ok(())
    }

    pub fn write_register(&mut self, name: &str, value: &str) -> Result<()> {
        let reg = name.trim_start_matches('$');
        let cmd = format!("set ${reg} = {value}");
        self.cmd(&format!("-interpreter-exec console \"{}\"", escape(&cmd)))?;
        Ok(())
    }

    pub fn reset(&mut self, halt: bool) -> Result<()> {
        let cmd = if halt {
            "monitor reset halt"
        } else {
            "monitor reset run"
        };
        self.cmd(&format!("-interpreter-exec console \"{cmd}\""))?;
        self.cmd("-interpreter-exec console \"maintenance flush register-cache\"")?;
        Ok(())
    }

    pub fn detach(&mut self) -> Result<()> {
        let _ = self.cmd("-target-detach");
        let _ = self.stdin.write_all(b"-gdb-exit\n");
        let _ = self.stdin.flush();
        // Bound the exit wait: gdb can wedge waiting on a dead remote, and
        // an unbounded child.wait() hangs the whole run.
        let deadline = std::time::Instant::now() + Duration::from_millis(self.timeout_ms);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        Ok(())
    }

    fn send(&mut self, cmd: &str) -> Result<()> {
        self.stdin
            .write_all(format!("{cmd}\n").as_bytes())
            .map_err(|e| ElabError::Machine(format!("gdb write: {e}")))?;
        self.stdin
            .flush()
            .map_err(|e| ElabError::Machine(format!("gdb flush: {e}")))
    }

    fn cmd(&mut self, cmd: &str) -> Result<Record> {
        self.send(cmd)?;
        loop {
            let line = self.line()?;
            if line.starts_with('^') {
                let r = parse(&line);
                if r.kind == '^' && line.starts_with("^error") {
                    return Err(ElabError::Machine(
                        r.get("msg").unwrap_or("gdb error").to_string(),
                    ));
                }
                return Ok(r);
            }
        }
    }

    fn line(&mut self) -> Result<String> {
        use std::sync::mpsc::RecvTimeoutError;
        let timeout = Duration::from_millis(self.timeout_ms);
        match self.lines.recv_timeout(timeout) {
            Ok(s) => Ok(s),
            Err(RecvTimeoutError::Timeout) => Err(ElabError::Machine(format!(
                "gdb produced no output within {timeout:?} (hung target or wedged server?)"
            ))),
            Err(RecvTimeoutError::Disconnected) => {
                Err(ElabError::Machine("gdb closed the connection".into()))
            }
        }
    }
}

impl Machine for Gdb {
    fn watch(&mut self, expr: &str) -> Result<WatchHit> {
        Gdb::watch(self, expr)
    }
    fn backtrace(&mut self) -> Result<String> {
        Gdb::backtrace(self)
    }
    fn stop(&mut self, addr: u64, cond: Option<&str>) -> Result<u64> {
        self.stop_at(addr, cond)
    }
    fn evaluate(&mut self, expr: &str) -> Result<Value> {
        Gdb::evaluate(self, expr)
    }
    fn read_memory(&mut self, addr: u64, len: usize) -> Result<Vec<u8>> {
        Gdb::read_memory(self, addr, len)
    }
    fn write_memory(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        Gdb::write_memory(self, addr, bytes)
    }
    fn write_register(&mut self, name: &str, value: &str) -> Result<()> {
        Gdb::write_register(self, name, value)
    }
    fn reset(&mut self, halt: bool) -> Result<()> {
        Gdb::reset(self, halt)
    }
    fn detach(&mut self) -> Result<()> {
        Gdb::detach(self)
    }
    fn set_timeout_ms(&mut self, ms: u64) {
        self.timeout_ms = ms;
    }
    fn flash_read(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        Gdb::flash_read_via_monitor(self, offset, len)
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn parse(line: &str) -> Record {
    let kind = line.chars().next().unwrap_or(' ');
    let body = if matches!(kind, '^' | '*' | '=' | '~' | '&') {
        &line[1..]
    } else {
        line
    };
    Record {
        kind,
        fields: fields(body),
        raw: line.to_string(),
    }
}

fn fields(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut depth = 0i32;
    let mut in_q = false;
    let mut esc = false;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if in_q {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_q = false;
            }
            continue;
        }
        match b {
            b'"' => in_q = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(pair(&body[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(pair(&body[start..]));
    out
}

fn pair(s: &str) -> (String, String) {
    match s.split_once('=') {
        Some((k, v)) => (k.trim().to_string(), unquote(v.trim())),
        None => (s.trim().to_string(), String::new()),
    }
}

fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => out.push(other),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        v.to_string()
    }
}

fn nested(value: Option<&str>, key: &str) -> Option<String> {
    let inner = value?.strip_prefix('{')?.strip_suffix('}')?;
    fields(inner)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

fn watch_hit(record: &Record, number: &str) -> Result<WatchHit> {
    if record.get("reason") != Some("watchpoint-trigger")
        || nested(record.get("wpt"), "number").as_deref() != Some(number)
    {
        return Err(ElabError::Machine(format!(
            "unexpected stop while watching {number}: {}",
            record.raw
        )));
    }
    let required = |value: Option<String>, field: &str| {
        value.ok_or_else(|| {
            ElabError::Machine(format!("watchpoint stop missing {field}: {}", record.raw))
        })
    };
    let addr = required(nested(record.get("frame"), "addr"), "frame.addr")?;
    Ok(WatchHit {
        pc: Value::from_gdb(&addr).as_u64()?,
        reason: "watchpoint-trigger".into(),
        old: required(nested(record.get("value"), "old"), "value.old")?,
        new: required(nested(record.get("value"), "new"), "value.new")?,
        raw: record.raw.clone(),
    })
}
