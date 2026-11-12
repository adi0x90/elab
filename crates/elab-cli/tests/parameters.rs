use std::process::Command;
#[test]
fn parameters_are_validated_without_opening_transport() {
    let dir = std::env::temp_dir().join(format!("elab-parameters-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("device.toml"), "name = 'test'\n[image]\n[stops]\n").unwrap();
    let spec = dir.join("experiment.yaml");
    std::fs::write(&spec,"name: test\ndevice: device.toml\ntransport: {}\nparameters:\n  address: {kind: address, default: '0x1000'}\n  value: {kind: u8}\nphases: []\n").unwrap();
    for (args, valid, message) in [
        (vec!["--set", "value=255"], true, "spec ok"),
        (vec!["--set", "value=256"], false, "parameter 'value'"),
        (
            vec!["--set", "value=0", "--set", "address=0x2000"],
            true,
            "spec ok",
        ),
        (vec!["--set", "unknown=1"], false, "unknown parameter"),
        (
            vec!["--set", "value=0", "--set", "value=1"],
            false,
            "duplicate parameter",
        ),
        (vec![], false, "parameter 'value'"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_elab"))
            .arg("check")
            .arg(&spec)
            .args(args)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.success(), valid, "{text}");
        assert!(text.contains(message), "{text}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
