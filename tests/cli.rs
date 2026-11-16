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
