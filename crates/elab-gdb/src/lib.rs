//! GDB/MI transport for elab.
//!
//! GDB stays the expression and breakpoint engine. This module starts one MI
//! session, inserts hardware temporary breakpoints, waits for `*stopped`, and
//! evaluates GDB expressions (for example `*(float*)($sp+12)`).

use elab_core::{ElabError, Machine, Result, Value};
use regex::Regex;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct Gdb {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
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
            reader: BufReader::new(stdout),
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

    pub fn reset(&mut self, halt: bool) -> Result<()> {
        let cmd = if halt {
            "monitor reset halt"
        } else {
            "monitor reset run"
        };
        self.cmd(&format!("-interpreter-exec console \"{cmd}\""))?;
        Ok(())
    }

    pub fn detach(&mut self) -> Result<()> {
        let _ = self.cmd("-target-detach");
        let _ = self.stdin.write_all(b"-gdb-exit\n");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
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
        let mut s = String::new();
        let n = self
            .reader
            .read_line(&mut s)
            .map_err(|e| ElabError::Machine(format!("gdb read: {e}")))?;
        if n == 0 {
            return Err(ElabError::Machine("gdb closed the connection".into()));
        }
        Ok(s.trim_end().to_string())
    }
}

impl Machine for Gdb {
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
    fn reset(&mut self, halt: bool) -> Result<()> {
        Gdb::reset(self, halt)
    }
    fn detach(&mut self) -> Result<()> {
        Gdb::detach(self)
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
