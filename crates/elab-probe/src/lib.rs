//! probe-rs `Machine` backend.
//!
//! This experimental backend covers attach, hardware breakpoints, memory access and
//! reset. Expression evaluation is limited to `$pc` and numeric literals; use
//! the GDB backend for symbol-rich expressions such as `stats.input`.

use elab_core::{ElabError, Machine, Result, Value};
use probe_rs::{Core, MemoryInterface, Session, SessionConfig};
use std::time::Duration;

pub struct ProbeMachine {
    session: Session,
    core_index: usize,
    timeout: Duration,
}

impl ProbeMachine {
    pub fn new(chip: &str, timeout_ms: u64) -> Result<Self> {
        let session = Session::auto_attach(chip, SessionConfig::default())
            .map_err(|e| ElabError::Machine(format!("probe attach {chip}: {e}")))?;
        Ok(Self {
            session,
            core_index: 0,
            timeout: Duration::from_millis(timeout_ms),
        })
    }

    fn core(&mut self) -> Result<Core<'_>> {
        self.session
            .core(self.core_index)
            .map_err(|e| ElabError::Machine(format!("probe core: {e}")))
    }

    fn pc(&mut self) -> Result<u64> {
        let mut c = self.core()?;
        let pc: u32 = c
            .read_core_reg(c.program_counter().id())
            .map_err(|e| ElabError::Machine(format!("probe pc: {e}")))?;
        Ok(pc as u64)
    }
}

impl Machine for ProbeMachine {
    fn stop(&mut self, addr: u64, cond: Option<&str>) -> Result<u64> {
        if cond.is_some() {
            return Err(ElabError::msg(
                "probe backend does not support conditional stops yet",
            ));
        }
        {
            let timeout = self.timeout;
            let mut c = self.core()?;
            c.set_hw_breakpoint(addr)
                .map_err(|e| ElabError::Machine(format!("probe breakpoint: {e}")))?;
            c.run()
                .map_err(|e| ElabError::Machine(format!("probe run: {e}")))?;
            c.wait_for_core_halted(timeout)
                .map_err(|e| ElabError::Machine(format!("probe wait: {e}")))?;
            let _ = c.clear_hw_breakpoint(addr);
        }
        let pc = self.pc()?;
        if pc != addr {
            return Err(ElabError::Machine(format!(
                "stopped at 0x{pc:x}, expected 0x{addr:x}"
            )));
        }
        Ok(pc)
    }

    fn evaluate(&mut self, expr: &str) -> Result<Value> {
        let e = expr.trim();
        if e == "$pc" {
            return Ok(Value::UInt(self.pc()?));
        }
        if let Some(h) = e.strip_prefix("0x") {
            if let Ok(v) = u64::from_str_radix(h, 16) {
                return Ok(Value::UInt(v));
            }
        }
        if let Ok(v) = e.parse::<i64>() {
            return Ok(Value::Int(v));
        }
        if let Ok(f) = e.parse::<f64>() {
            return Ok(Value::Float(f));
        }
        Err(ElabError::msg(format!(
            "probe backend cannot evaluate '{e}': use $pc, a number, or the GDB transport"
        )))
    }

    fn read_memory(&mut self, addr: u64, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let mut c = self.core()?;
        c.read(addr, &mut buf)
            .map_err(|e| ElabError::Machine(format!("probe read 0x{addr:x}: {e}")))?;
        Ok(buf)
    }

    fn write_memory(&mut self, addr: u64, bytes: &[u8]) -> Result<()> {
        let mut c = self.core()?;
        c.write(addr, bytes)
            .map_err(|e| ElabError::Machine(format!("probe write 0x{addr:x}: {e}")))?;
        Ok(())
    }

    fn reset(&mut self, halt: bool) -> Result<()> {
        let timeout = self.timeout;
        let mut c = self.core()?;
        if halt {
            c.reset_and_halt(timeout)
                .map_err(|e| ElabError::Machine(format!("probe reset halt: {e}")))?;
        } else {
            c.reset()
                .map_err(|e| ElabError::Machine(format!("probe reset: {e}")))?;
        }
        Ok(())
    }

    fn detach(&mut self) -> Result<()> {
        Ok(())
    }
}
