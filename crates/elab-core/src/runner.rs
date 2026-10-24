use crate::device::{Device, Endian};
use crate::spec::{ExperimentSpec, Op};
use crate::{sha256_hex, ElabError, Evidence, Machine, Result, SerialLink, Value};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct PublishedRow {
    pub phase: String,
    pub x: f64,
    pub values: BTreeMap<String, f64>,
    pub count: u64,
}

#[derive(Debug, Default)]
pub struct RunResult {
    pub rows: Vec<PublishedRow>,
    pub failures: Vec<String>,
    pub ok: bool,
    pub channels: Vec<String>,
}

struct Runner<'a> {
    device: &'a Device,
    machine: &'a mut dyn Machine,
    serial: Option<&'a mut dyn SerialLink>,
    ev: &'a mut Evidence,
    vars: HashMap<String, Value>,
    last: Option<PublishedRow>,
    rows: Vec<PublishedRow>,
    failures: Vec<String>,
    saves: Vec<(u64, Vec<u8>)>,
}

pub fn run<'a>(
    spec: &'a ExperimentSpec,
    device: &'a Device,
    machine: &'a mut dyn Machine,
    serial: Option<&'a mut dyn SerialLink>,
    ev: &'a mut Evidence,
) -> Result<RunResult> {
    let channels = device.channel_names();
    let mut r = Runner {
        device,
        machine,
        serial,
        ev,
        vars: HashMap::new(),
        last: None,
        rows: Vec::new(),
        failures: Vec::new(),
        saves: Vec::new(),
    };
    let res = r.exec(spec);
    if res.is_err() {
        for (addr, old) in r.saves.iter().rev() {
            let _ = r.machine.write_memory(*addr, old);
        }
    }
    res?;
    let ok = r.failures.is_empty();
    Ok(RunResult {
        rows: r.rows,
        failures: r.failures,
        ok,
        channels,
    })
}

impl<'a> Runner<'a> {
    fn exec(&mut self, spec: &ExperimentSpec) -> Result<()> {
        for phase in &spec.phases {
            self.ev
                .event(json!({"event": "phase", "name": phase.name}))?;
            for op in &phase.ops {
                self.op(&phase.name, op)?;
            }
        }
        Ok(())
    }

    fn op(&mut self, phase: &str, op: &Op) -> Result<()> {
        match op {
            Op::Stop { at, when } => {
                let st = self.device.stop(at)?.clone();
                let cond = when.clone().or(st.cond.clone());
                let pc = self.machine.stop(st.addr, cond.as_deref())?;
                self.ev.event(json!({
                    "event": "stop", "phase": phase, "at": at,
                    "pc": format!("0x{pc:x}")
                }))?;
            }
            Op::Capture { name, expr } => {
                let e = subst(expr, &self.vars);
                let v = self.machine.evaluate(&e)?;
                self.ev.event(json!({
                    "event": "capture", "phase": phase, "name": name,
                    "expr": expr, "value": v.literal()
                }))?;
                self.vars.insert(name.clone(), v);
            }
            Op::Write { at, kind, value } => {
                let addr = self.address(at)?;
                let v_s = subst(value, &self.vars);
                let v = self.machine.evaluate(&v_s)?;
                let bytes = encode(&v, kind, self.device.endian)?;
                let old = self.machine.read_memory(addr, bytes.len())?;
                self.saves.push((addr, old.clone()));
                self.machine.write_memory(addr, &bytes)?;
                let now = self.machine.read_memory(addr, bytes.len())?;
                if now != bytes {
                    return Err(ElabError::msg(format!("write at 0x{addr:x} did not stick")));
                }
                self.ev.event(json!({
                    "event": "write", "phase": phase, "addr": format!("0x{addr:x}"),
                    "kind": kind, "before": hex::encode(&old), "after": hex::encode(&bytes)
                }))?;
            }
            Op::Published {
                stop,
                match_expr,
                tol,
                tries,
            } => {
                let st = self.device.stop(stop)?.clone();
                let published = self.device.published.as_ref().ok_or_else(|| {
                    ElabError::msg("this device profile has no [published] section")
                })?;
                let x_expr = published.x.clone();
                let count_expr = published.count.clone();
                let channels = self.device.channels.clone();
                let want_expr = subst(match_expr, &self.vars);
                let want = self.machine.evaluate(&want_expr)?.as_f64()?;
                let mut found = None;
                for _ in 0..*tries {
                    self.machine.stop(st.addr, st.cond.as_deref())?;
                    let x = self.machine.evaluate(&x_expr)?.as_f64()?;
                    if (x - want).abs() <= *tol {
                        let count = self.machine.evaluate(&count_expr)?.as_u64()?;
                        let mut values = BTreeMap::new();
                        for ch in &channels {
                            let v = self.machine.evaluate(&ch.value)?.as_f64()?;
                            values.insert(ch.name.clone(), v);
                        }
                        found = Some(PublishedRow {
                            phase: phase.to_string(),
                            x,
                            values,
                            count,
                        });
                        break;
                    }
                }
                let row = found.ok_or_else(|| {
                    ElabError::msg(format!(
                        "no published sample matching {want} for phase {phase}"
                    ))
                })?;
                self.ev.event(json!({
                    "event": "published", "phase": phase, "x": row.x,
                    "count": row.count, "values": row.values
                }))?;
                self.vars.insert("x".into(), Value::Float(row.x));
                self.vars.insert("count".into(), Value::UInt(row.count));
                for (k, v) in &row.values {
                    self.vars.insert(k.clone(), Value::Float(*v));
                }
                self.last = Some(row.clone());
                self.rows.push(row);
            }
            Op::Expect { fields } => {
                let row = self
                    .last
                    .clone()
                    .ok_or_else(|| ElabError::msg("expect without a published row"))?;
                for (k, want) in fields {
                    let got = if k == "x" {
                        row.x
                    } else if k == "count" {
                        row.count as f64
                    } else {
                        *row.values
                            .get(k)
                            .ok_or_else(|| ElabError::msg(format!("unknown expect field {k}")))?
                    };
                    let ok = (got - *want).abs() <= 1e-6;
                    self.ev.event(json!({
                        "event": "expect", "phase": phase, "field": k,
                        "want": want, "got": got, "ok": ok
                    }))?;
                    if !ok {
                        self.failures
                            .push(format!("{phase}.{k}: got {got} want {want}"));
                    }
                }
            }
            Op::SerialExpect {
                pattern,
                timeout_ms,
            } => {
                let s = match self.serial.as_mut() {
                    Some(s) => &mut **s,
                    None => return Err(ElabError::msg("serial op without a serial transport")),
                };
                let text = s.expect(pattern, Duration::from_millis(*timeout_ms))?;
                self.ev.event(json!({
                    "event": "serial_expect", "phase": phase, "pattern": pattern, "text": text
                }))?;
            }
            Op::SerialSend { text } => {
                let s = match self.serial.as_mut() {
                    Some(s) => &mut **s,
                    None => return Err(ElabError::msg("serial op without a serial transport")),
                };
                s.send(text)?;
                self.ev.event(json!({
                    "event": "serial_send", "phase": phase, "text": text
                }))?;
            }
            Op::Reset { halt } => {
                self.machine.reset(*halt)?;
                self.ev.event(json!({
                    "event": "reset", "phase": phase, "halt": halt
                }))?;
            }
            Op::PowerCycle => {
                let argv = self
                    .device
                    .power
                    .as_ref()
                    .and_then(|p| p.cycle.as_ref())
                    .ok_or_else(|| ElabError::msg("profile has no [power] cycle command"))?;
                if argv.is_empty() {
                    return Err(ElabError::msg("[power] cycle is empty"));
                }
                let status = std::process::Command::new(&argv[0])
                    .args(&argv[1..])
                    .status()
                    .map_err(|e| ElabError::msg(format!("power cycle: {e}")))?;
                self.ev.event(json!({
                    "event": "power_cycle", "phase": phase,
                    "argv": argv, "ok": status.success()
                }))?;
                if !status.success() {
                    return Err(ElabError::msg(format!("power cycle exited with {status}")));
                }
            }
            Op::Attachment { name, at, len } => {
                let addr = self.address(at)?;
                let bytes = self.machine.read_memory(addr, *len)?;
                let dir = self.ev.dir().join("attachments");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{name}.bin"));
                std::fs::write(&path, &bytes)?;
                let sha = sha256_hex(&bytes);
                self.ev.event(json!({
                    "event": "attachment", "phase": phase, "name": name,
                    "addr": format!("0x{addr:x}"), "len": len,
                    "path": path.display().to_string(), "sha256": sha
                }))?;
            }
        }
        Ok(())
    }

    fn address(&mut self, expr: &str) -> Result<u64> {
        let s = subst(expr, &self.vars);
        let t = s.trim();
        if let Some(h) = t.strip_prefix("0x") {
            if let Ok(v) = u64::from_str_radix(h, 16) {
                return Ok(v);
            }
        }
        if let Ok(v) = t.parse::<u64>() {
            return Ok(v);
        }
        self.machine
            .evaluate(&format!("(unsigned long)({s})"))?
            .as_u64()
    }
}

fn subst(s: &str, vars: &HashMap<String, Value>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        if let Some(j) = after.find('}') {
            let name = &after[..j];
            match vars.get(name) {
                Some(v) => out.push_str(&v.literal()),
                None => out.push_str(&rest[i..i + 2 + j + 1]),
            }
            rest = &after[j + 1..];
        } else {
            out.push_str(&rest[i..]);
            return out;
        }
    }
    out.push_str(rest);
    out
}

fn encode(v: &Value, kind: &str, endian: Endian) -> Result<Vec<u8>> {
    let f = v.as_f64()?;
    let u = v.as_u64();
    let bytes32 = |b: [u8; 4]| match endian {
        Endian::Little => b.to_vec(),
        Endian::Big => {
            let mut r = b.to_vec();
            r.reverse();
            r
        }
    };
    match kind {
        "f32" => Ok(bytes32((f as f32).to_le_bytes())),
        "f64" => {
            let b = f.to_le_bytes();
            Ok(match endian {
                Endian::Little => b.to_vec(),
                Endian::Big => {
                    let mut r = b.to_vec();
                    r.reverse();
                    r
                }
            })
        }
        "i8" => Ok((f as i8).to_le_bytes().to_vec()),
        "u8" => Ok((u? as u8).to_le_bytes().to_vec()),
        "u32" => Ok(bytes32((u? as u32).to_le_bytes())),
        "i32" => Ok(bytes32((f as i32).to_le_bytes())),
        other => Err(ElabError::msg(format!("unknown write kind '{other}'"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_leaves_gdb_registers_alone() {
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), Value::Float(1.5));
        assert_eq!(subst("${x} + 0.05", &vars), "1.5 + 0.05");
        assert_eq!(subst("$sp+12", &vars), "$sp+12");
    }

    #[test]
    fn encoding_uses_width_and_endianness() {
        assert_eq!(
            encode(&Value::Float(1.0), "f32", Endian::Little).unwrap(),
            1.0f32.to_le_bytes()
        );
        assert_eq!(
            encode(&Value::Float(1.0), "f32", Endian::Big).unwrap(),
            1.0f32.to_be_bytes()
        );
        assert_eq!(
            encode(&Value::Int(-128), "i8", Endian::Big).unwrap(),
            vec![0x80u8]
        );
        assert!(encode(&Value::Float(1.0), "nope", Endian::Little).is_err());
    }
}
