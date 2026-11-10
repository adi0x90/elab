#![cfg(unix)]
use elab_core::Machine;
use elab_gdb::Gdb;
use std::os::unix::fs::PermissionsExt;

#[test]
fn watch_validates_stop_and_removes_only_its_own_watchpoint() {
    let dir = std::env::temp_dir().join(format!("elab-mi-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("gdb");
    let log = dir.join("commands");
    std::fs::write(&script, r##"#!/bin/sh
while IFS= read -r cmd; do
  printf '%s\n' "$cmd" >> "$ELAB_TEST_LOG"
  case "$cmd" in
    '-break-watch '*) printf '%s\n' '^done,wpt={number="7",exp="nn_act[4]"}' ;;
    '-break-info 7')
      if [ "$ELAB_TEST_MODE" = soft ]; then kind='watchpoint'; else kind='hw watchpoint'; fi
      printf '^done,BreakpointTable={body=[bkpt={number="7",type="%s"}]}\n' "$kind" ;;
    '-exec-continue')
      case "$ELAB_TEST_MODE" in
        timeout) printf '%s\n' '^running' ;;
        error) printf '%s\n' '^error,msg="Cannot resume target"' ;;
        unrelated) printf '%s\n' '^running' '*stopped,reason="breakpoint-hit",bkptno="9",frame={addr="0x420094e2"}' ;;
        wrong_watch) printf '%s\n' '^running' '*stopped,reason="watchpoint-trigger",wpt={number="8"},value={old="25",new="26"},frame={addr="0x420094e2"}' ;;
        *) printf '%s\n' '^running' '*stopped,reason="watchpoint-trigger",wpt={number="7",exp="nn_act[4]"},value={old="25",new="26"},frame={addr="0x420094e2",func="nn_infer",args=[]},thread-id="2"' ;;
      esac ;;
    '-stack-list-frames') printf '%s\n' '^done,stack=[frame={level="0",addr="0x420094e2",func="nn_infer"},frame={level="1",func="inference_task"}]' ;;
    '-gdb-exit') exit 0 ;;
    *) printf '%s\n' '^done' ;;
  esac
done
"##).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("ELAB_GDB", &script);
    std::env::set_var("ELAB_TEST_LOG", &log);
    for mode in [
        "success",
        "unrelated",
        "wrong_watch",
        "error",
        "soft",
        "timeout",
    ] {
        std::env::set_var("ELAB_TEST_MODE", mode);
        std::fs::write(&log, "").unwrap();
        let mut gdb = Gdb::new("unused.elf").unwrap();
        if mode == "timeout" {
            gdb.set_timeout_ms(250);
        }
        let result = gdb.watch("nn_act[4]");
        if mode == "success" {
            let hit = result.unwrap();
            assert_eq!(hit.pc, 0x420094e2);
            assert_eq!(hit.old, "25");
            assert_eq!(hit.new, "26");
            assert!(hit.raw.contains("thread-id=\"2\""));
            let stack = gdb.backtrace().unwrap();
            assert!(stack.contains("nn_infer") && stack.contains("inference_task"));
            gdb.reset(true).unwrap();
        } else {
            assert!(result.is_err(), "must reject {mode}");
            if mode == "timeout" {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("gdb produced no output within"));
            }
        }
        gdb.detach().unwrap();
        let commands = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            commands
                .lines()
                .filter(|c| c.starts_with("-break-delete"))
                .collect::<Vec<_>>(),
            vec!["-break-delete 7"]
        );
        if mode == "soft" {
            assert!(!commands.contains("-exec-continue"));
        }
        if mode == "success" {
            assert!(commands.contains("maintenance flush register-cache"));
        }
    }
    for key in ["ELAB_GDB", "ELAB_TEST_LOG", "ELAB_TEST_MODE"] {
        std::env::remove_var(key);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
