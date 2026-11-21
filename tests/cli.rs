use assert_cmd::Command;

#[test]
fn scan_on_empty_home_exits_zero() {
    let home = tempfile::tempdir().unwrap();
    Command::cargo_bin("dredge")
        .unwrap()
        .args(["scan", "--home"])
        .arg(home.path())
        .assert()
        .success();
}

#[test]
fn help_lists_flags() {
    let out = Command::cargo_bin("dredge")
        .unwrap()
        .args(["scan", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--dotfiles", "--fail-on", "--layout", "--json", "--all"] {
        assert!(text.contains(flag), "missing {flag} in help");
    }
}

#[test]
fn closed_stdout_pipe_exits_with_computed_code_silently() {
    use std::process::Stdio;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // Enough units that the JSON report is far larger than a pipe buffer, so
    // the write always hits the closed pipe.
    for i in 0..600 {
        let p = home.join(format!(".config/unk{i:03}"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x = 1\n").unwrap();
    }
    // One fake secret so the computed exit code is 1, not 0.
    let ngrok = home.join(".config/ngrok/ngrok.yml");
    std::fs::create_dir_all(ngrok.parent().unwrap()).unwrap();
    std::fs::write(&ngrok, format!("authtoken: {}\n", "a1".repeat(15))).unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("dredge"))
        .args(["scan", "--json", "--all", "--home"])
        .arg(&home)
        .current_dir(tmp.path())
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.stderr.is_empty(), "stderr was not empty");
    assert_eq!(out.status.code(), Some(1));
}
