#![cfg(unix)]
use elab_core::Machine;
use elab_gdb::Gdb;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

#[test]
fn resume_acknowledges_running_and_halt_waits_for_stop() {
    let dir = std::env::temp_dir().join(format!("elab-timed-mi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("gdb");
    let log = dir.join("commands");
    std::fs::write(
        &script,
        r##"#!/bin/sh
# SYNTHETIC GDB/MI protocol fixture, not hardware evidence.
while IFS= read -r cmd; do
  printf '%s\n' "$cmd" >> "$ELAB_TEST_LOG"
  case "$cmd" in
    '-exec-continue')
      if [ "$ELAB_TEST_MODE" = error ]; then
        printf '%s\n' '^error,msg="Cannot resume target"'
      else
        printf '%s\n' '^running' '*running,thread-id="all"'
      fi ;;
    '-exec-interrupt --all')
      printf '%s\n' '^done'
      sleep 0.1
      printf '%s\n' '*stopped,reason="signal-received",frame={addr="0x42000000"}' ;;
    '-data-evaluate-expression '*) printf '%s\n' '^done,value="42"' ;;
    '-gdb-exit') exit 0 ;;
    *) printf '%s\n' '^done' ;;
  esac
done
"##,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("ELAB_GDB", &script);
    std::env::set_var("ELAB_TEST_LOG", &log);
    for mode in ["success", "error"] {
        std::env::set_var("ELAB_TEST_MODE", mode);
        let mut gdb = Gdb::new("unused.elf").unwrap();
        gdb.set_timeout_ms(500);
        let result = Machine::resume(&mut gdb);
        if mode == "error" {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("Cannot resume target"));
        } else {
            result.expect("resume must return at ^running, without waiting for *stopped");
            let start = Instant::now();
            Machine::halt(&mut gdb).unwrap();
            assert!(start.elapsed() >= Duration::from_millis(90));
            assert_eq!(gdb.evaluate("word").unwrap().as_u64().unwrap(), 42);
            // A second cycle must not consume the first cycle's stop.
            Machine::resume(&mut gdb).unwrap();
            let start = Instant::now();
            Machine::halt(&mut gdb).unwrap();
            assert!(start.elapsed() >= Duration::from_millis(90));
        }
        gdb.detach().unwrap();
    }
    let commands = std::fs::read_to_string(log).unwrap();
    assert!(commands.contains("-gdb-set mi-async on"));
    assert!(commands.contains("maintenance flush register-cache"));
    std::env::remove_var("ELAB_GDB");
    std::env::remove_var("ELAB_TEST_LOG");
    std::env::remove_var("ELAB_TEST_MODE");
    std::fs::remove_dir_all(dir).unwrap();
}
