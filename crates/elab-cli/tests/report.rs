use serde_json::json;
use std::process::Command;
#[test]
fn report_preserves_repeated_observations_and_marks_missing() {
    let dir = std::env::temp_dir().join(format!("elab-report-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let events = [
        json!({"event":"phase","name":"baseline"}),
        json!({"event":"observation","phase":"baseline","name":"byte","occurrence":1,"value":{"type":"scalar","value":{"UInt":7}}}),
        json!({"event":"observation","phase":"baseline","name":"byte","occurrence":2,"value":{"type":"scalar","value":{"UInt":8}}}),
        json!({"event":"phase","name":"mutation"}),
        json!({"event":"observation","phase":"mutation","name":"byte","occurrence":1,"value":{"type":"scalar","value":{"UInt":0}}}),
    ];
    std::fs::write(
        dir.join("events.jsonl"),
        events.iter().map(|e| format!("{e}\n")).collect::<String>(),
    )
    .unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({"ok":true,"events":5}).to_string(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_elab"))
        .arg("report")
        .arg(&dir)
        .args(["--format", "markdown"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("| byte#1 | 7 | 0"), "{text}");
    assert!(text.contains("| byte#2 | 8 | missing |"), "{text}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn verify_recomputes_comparison_instead_of_trusting_pass_flag() {
    let dir = std::env::temp_dir().join(format!("elab-compare-verify-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let events = [
        json!({"event":"observation","phase":"baseline","name":"value","occurrence":1,"value":{"type":"scalar","value":{"UInt":7}}}),
        json!({"event":"observation","phase":"mutation","name":"value","occurrence":1,"value":{"type":"scalar","value":{"UInt":0}}}),
        json!({"event":"compare","phase":"mutation","left":"baseline.value","right":"mutation.value","relation":"equal","tolerance":0,"ok":true}),
    ];
    std::fs::write(
        dir.join("events.jsonl"),
        events.iter().map(|e| format!("{e}\n")).collect::<String>(),
    )
    .unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        json!({"ok":true,"events":3}).to_string(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_elab"))
        .arg("verify")
        .arg(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("comparison failed"));
    std::fs::remove_dir_all(dir).unwrap();
}
