//! OpenOCD server lifecycle for elab.
//!
//! Verifies an OpenOCD is listening before a run, starts one when the spec
//! asks (`start: true`, locally or over ssh), tunnels remote ports, and stops
//! only the instance elab itself spawned. Also detects what holds a serial
//! device open (picocom, minicom, ...) with a clear error instead of a
//! cryptic "permission denied" or "device busy" from openocd.

use crate::spec::OpenOcdSpec;
use crate::{ElabError, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A managed OpenOCD instance. Dropping it kills the process elab spawned
/// (locally or remotely); an already-running server is left alone.
pub struct OpenOcdHandle {
    /// Local openocd process (spawned locally) — killed on drop.
    local_child: Option<Child>,
    /// Local `ssh -N -L` tunnel child (remote host) — killed on drop.
    tunnel: Option<Child>,
    /// Remote host (user@host form) when openocd runs over ssh.
    ssh_target: Option<String>,
    /// Remote pid file written when elab started the remote openocd.
    remote_pid_file: Option<String>,
    spawned: bool,
}

impl Drop for OpenOcdHandle {
    fn drop(&mut self) {
        if !self.spawned {
            return;
        }
        if let Some(c) = self.local_child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(c) = self.tunnel.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let (Some(target), Some(pid_file)) = (&self.ssh_target, &self.remote_pid_file) {
            let _ = Command::new("ssh")
                .arg(target)
                .arg(format!(
                    "kill $(cat {pid_file}) 2>/dev/null; rm -f {pid_file}"
                ))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// A small telnet-style TCP client that strips IAC negotiation and collects
/// output until the OpenOCD prompt (`> `) or a deadline.
pub struct Telnet {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Telnet {
    pub fn connect(host: &str, port: u16, timeout_ms: u64) -> Result<Self> {
        let addr = format!("{host}:{port}");
        let stream = TcpStream::connect(&addr)
            .map_err(|e| ElabError::msg(format!("cannot reach openocd telnet {addr}: {e}")))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(timeout_ms.max(100))))
            .map_err(|e| ElabError::msg(format!("telnet set timeout: {e}")))?;
        Ok(Self {
            stream,
            buf: Vec::new(),
        })
    }

    /// Read bytes, stripping telnet IAC sequences (0xFF 0xFB/0xFC/0xFD/0xFE/...)
    /// that OpenOCD's telnet server negotiates at connect.
    fn recv_strip(&mut self) -> Result<Vec<u8>> {
        let mut raw = [0u8; 4096];
        let n = self
            .stream
            .read(&mut raw)
            .map_err(|e| ElabError::msg(format!("telnet read: {e}")))?;
        if n == 0 {
            return Ok(Vec::new());
        }
        // Filter IAC: FF followed by FB|FC|FD|FE (+ option byte), or FF F4/F5/F6.
        let mut out = Vec::with_capacity(n);
        let mut i = 0;
        while i < n {
            if raw[i] == 0xFF && i + 1 < n {
                let cmd = raw[i + 1];
                if (0xFA..=0xFE).contains(&cmd) {
                    i += 3;
                    continue;
                }
                i += 2;
                continue;
            }
            out.push(raw[i]);
            i += 1;
        }
        Ok(out)
    }

    /// Read until output contains `needle` or the deadline passes. Returns
    /// everything received (IAC-stripped).
    pub fn read_until(&mut self, needle: &str, deadline_ms: u64) -> Result<String> {
        let start = Instant::now();
        let mut acc = self.buf.clone();
        loop {
            if String::from_utf8_lossy(&acc).contains(needle) {
                self.buf = Vec::new();
                return Ok(String::from_utf8_lossy(&acc).into_owned());
            }
            if start.elapsed().as_millis() as u64 > deadline_ms {
                self.buf = Vec::new();
                return Ok(String::from_utf8_lossy(&acc).into_owned());
            }
            let chunk = self.recv_strip()?;
            acc.extend_from_slice(&chunk);
        }
    }

    /// Send one command and collect output up to the prompt.
    pub fn cmd(&mut self, cmd: &str, deadline_ms: u64) -> Result<String> {
        self.stream
            .write_all(format!("{cmd}\n").as_bytes())
            .map_err(|e| ElabError::msg(format!("telnet write: {e}")))?;
        self.read_until("> ", deadline_ms)
    }
}

/// True when something is listening on host:port (TCP connect succeeds).
pub fn port_listening(host: &str, port: u16, timeout_ms: u64) -> bool {
    let addr = format!("{host}:{port}");
    match TcpStream::connect_timeout(
        &addr.parse().unwrap_or_else(|_| ([127, 0, 0, 1], 0).into()),
        Duration::from_millis(timeout_ms.max(50)),
    ) {
        Ok(s) => {
            let _ = s.shutdown(std::net::Shutdown::Both);
            true
        }
        Err(_) => false,
    }
}

/// Verify the telnet port answers with OpenOCD's banner. Prevents elab from
/// silently talking to some other service bound on the debug port.
pub fn telnet_is_openocd(host: &str, port: u16) -> bool {
    let Ok(mut t) = Telnet::connect(host, port, 3000) else {
        return false;
    };
    let banner = t
        .read_until("Open On-Chip Debugger", 3000)
        .unwrap_or_default();
    banner.contains("Open On-Chip Debugger")
}

fn ssh_target(spec: &OpenOcdSpec) -> Option<String> {
    let host = spec.host.as_deref()?;
    match &spec.user {
        Some(u) => Some(format!("{u}@{host}")),
        None => Some(host.to_string()),
    }
}

/// Processes (pid + name) holding `dev` open. Empty means free.
pub fn serial_port_holders(dev: &str, ssh_target: Option<&str>) -> Result<Vec<String>> {
    // lsof -F pcfn: leading letter tags (p=pid, c=comm, n=name); plain lines
    // combine into one record per process.
    let out = match ssh_target {
        Some(t) => Command::new("ssh")
            .arg(t)
            .arg(format!(
                "command -v lsof >/dev/null && lsof -F pcfn -- {dev} 2>/dev/null || true"
            ))
            .output()
            .map_err(|e| ElabError::msg(format!("ssh lsof: {e}")))?,
        None => Command::new("sh")
            .arg("-c")
            .arg(format!(
                "command -v lsof >/dev/null && lsof -F pcfn -- {dev} 2>/dev/null || true"
            ))
            .output()
            .map_err(|e| ElabError::msg(format!("lsof: {e}")))?,
    };
    let mut holders = Vec::new();
    let mut pid: Option<String> = None;
    let mut comm = String::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        match line.chars().next() {
            Some('p') => {
                // A new pid starts a record; flush the previous one.
                if let Some(p) = pid.take() {
                    let c = std::mem::take(&mut comm);
                    holders.push(format!("pid {p} ({c})"));
                }
                pid = Some(line[1..].trim().to_string());
            }
            Some('c') => comm = line[1..].trim().to_string(),
            _ => {}
        }
    }
    if let Some(p) = pid {
        let c = std::mem::take(&mut comm);
        holders.push(format!("pid {p} ({c})"));
    }
    Ok(holders)
}

/// Ensure an OpenOCD matching the spec is reachable. Returns a handle that
/// owns (and on drop, stops) any process elab spawned.
pub fn ensure_running(spec: &OpenOcdSpec) -> Result<OpenOcdHandle> {
    let target = ssh_target(spec);
    let gdb_host = match &target {
        Some(_) => "127.0.0.1", // via tunnel
        None => "127.0.0.1",
    };
    let remote_gdb = if target.is_some() {
        spec.gdb_port
    } else {
        spec.gdb_port
    };

    // 1. Something already listening?
    if port_listening(gdb_host, spec.gdb_port, 500) {
        // Distinguish openocd (reuse) from an impostor (error).
        let telnet_host = match &target {
            Some(_) => "127.0.0.1",
            None => "127.0.0.1",
        };
        let telnet = if target.is_some() {
            spec.telnet_port
        } else {
            spec.telnet_port
        };
        if telnet_is_openocd(telnet_host, telnet) {
            return Ok(OpenOcdHandle {
                local_child: None,
                tunnel: None,
                ssh_target: None,
                remote_pid_file: None,
                spawned: false,
            });
        }
        return Err(ElabError::msg(format!(
            "port {} is listening but does not speak OpenOCD; another process may be bound there",
            spec.gdb_port
        )));
    }

    // 2. Nothing listening. Start if asked; else fail with guidance.
    if !spec.start {
        return Err(ElabError::msg(format!(
            "no OpenOCD listening on port {}; start one (elab start=true) or run: openocd {}",
            spec.gdb_port,
            spec.config.join(" ")
        )));
    }

    let cfg_args: Vec<String> = spec
        .config
        .iter()
        .flat_map(|c| {
            if c.starts_with("-") {
                vec![c.clone()]
            } else {
                vec!["-f".to_string(), c.clone()]
            }
        })
        .collect();

    let handle = match &target {
        Some(t) => {
            // Remote: start openocd detached over ssh with a pid file, then
            // tunnel the gdb + telnet ports back to 127.0.0.1 locally.
            let pid_file = "/tmp/elab-openocd.pid";
            let log = "/tmp/elab-openocd.log";
            let remote_cmd = format!(
                "nohup openocd {} > {log} 2>&1 & echo $! > {pid_file}; sleep 1",
                cfg_args.join(" ")
            );
            let st = Command::new("ssh")
                .arg(t)
                .arg(remote_cmd)
                .status()
                .map_err(|e| ElabError::msg(format!("ssh openocd spawn: {e}")))?;
            if !st.success() {
                return Err(ElabError::msg(format!(
                    "openocd failed to start on {t}; see {log} on that host"
                )));
            }
            // Tunnel local spec ports -> remote ports.
            let local_gdb = spec.gdb_port;
            let local_telnet = spec.telnet_port;
            let child = Command::new("ssh")
                .arg("-N")
                .arg("-L")
                .arg(format!("{}:127.0.0.1:{}", local_gdb, remote_gdb))
                .arg("-L")
                .arg(format!("{}:127.0.0.1:{}", local_telnet, spec.telnet_port))
                .arg(t)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| ElabError::msg(format!("ssh tunnel spawn: {e}")))?;
            OpenOcdHandle {
                local_child: None,
                tunnel: Some(child),
                ssh_target: Some(t.clone()),
                remote_pid_file: Some(pid_file.to_string()),
                spawned: true,
            }
        }
        None => {
            // Local: spawn openocd, wait for the listening banner.
            let mut child = Command::new("openocd")
                .args(&cfg_args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| ElabError::msg(format!("openocd spawn: {e}")))?;
            let mut stdout = child.stdout.take();
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut started = false;
            if let Some(out) = stdout.as_mut() {
                let mut buf = [0u8; 512];
                let mut acc = String::new();
                while Instant::now() < deadline {
                    use std::io::Read;
                    let n = out.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if acc.contains("Listening on port") {
                        started = true;
                        break;
                    }
                }
            }
            if !started {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ElabError::msg(
                    "openocd did not report listening within 20s; check its config (device present? USB busy?)",
                ));
            }
            OpenOcdHandle {
                local_child: Some(child),
                tunnel: None,
                ssh_target: None,
                remote_pid_file: None,
                spawned: true,
            }
        }
    };

    // 3. Wait until the ports actually accept connections.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if port_listening("127.0.0.1", spec.gdb_port, 500)
            && port_listening("127.0.0.1", spec.telnet_port, 500)
        {
            break;
        }
        if Instant::now() > deadline {
            return Err(ElabError::msg(format!(
                "openocd started but ports {}/{} never opened",
                spec.gdb_port, spec.telnet_port
            )));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = remote_gdb;
    Ok(handle)
}
