//! Black-box safety suite. Every test runs the real `dredge` binary against a
//! fixture home in a temp dir (never the real home) and asserts a property the
//! design spec lists under "Safety rules".

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::Fixture;

const BIN: &str = env!("CARGO_BIN_EXE_dredge");

/// Seeded fake secrets, one per rule family. None may ever appear in output.
/// Values are never printed in failure messages, only their index.
const SECRETS: &[&str] = &[
    "-----BEGIN OPENSSH PRIVATE KEY-----",
    "MIIEvQIBADANBgkqZ9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7Vq",
    "AKIAZ7Q3M5XN2PLR8WTB",
    "ghp_Zk3Xq9Lm2Vb7Nc4Rt8Yw1Hs6Jd5Fg0Pa2QeU",
    "7Hq2Zk9XvB4mN8cL3rT6yW1pSdF5gJa",
];

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the binary with a deadline. A hang fails the test instead of blocking.
fn run_bin(args: &[&str], timeout: Duration) -> Run {
    // Structural "never the real home": cwd is the fixture root and HOME is the
    // fixture home, both derived from the --home argument every test passes.
    let home_arg = args
        .iter()
        .position(|a| *a == "--home")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap();
    let cwd = home_arg.parent().unwrap().to_path_buf();
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("HOME", &home_arg)
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = |mut r: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("dredge did not finish within {timeout:?} (hang)");
        }
        thread::sleep(Duration::from_millis(20));
    };
    Run {
        code: status.code(),
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

fn scan(args: &[&str]) -> Run {
    let mut v = vec!["scan"];
    v.extend_from_slice(args);
    run_bin(&v, Duration::from_secs(30))
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

struct Env {
    fx: Fixture,
}

impl Env {
    fn new() -> Env {
        let fx = Fixture::new();
        fs::create_dir_all(fx.root().join("home")).unwrap();
        Env { fx }
    }
    fn home(&self) -> PathBuf {
        self.fx.root().join("home")
    }
    fn dots(&self) -> PathBuf {
        self.fx.root().join("dots")
    }
    fn h(&self, rel: &str, contents: &str) -> PathBuf {
        let p = self.home().join(rel);
        write(&p, contents);
        p
    }
    fn d(&self, rel: &str, contents: &str) -> PathBuf {
        let p = self.dots().join(rel);
        write(&p, contents);
        p
    }
    fn home_s(&self) -> String {
        self.home().to_string_lossy().into_owned()
    }
    fn dots_s(&self) -> String {
        self.dots().to_string_lossy().into_owned()
    }
    /// Runs `dredge scan --home <home> --dotfiles <dots> <extra>`.
    fn scan(&self, extra: &[&str]) -> Run {
        let (h, d) = (self.home_s(), self.dots_s());
        let mut v = vec!["--home", h.as_str(), "--dotfiles", d.as_str()];
        v.extend_from_slice(extra);
        scan(&v)
    }
}

/// Everything about a path that a read-only tool must not change.
#[derive(Debug, PartialEq, Eq)]
struct Snap {
    kind: String,
    size: u64,
    mtime: (i64, i64),
    mode: u32,
    /// File bytes, symlink target, or sorted directory entries.
    content: Vec<u8>,
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Snap> {
    let mut out = BTreeMap::new();
    walk_snap(root, &mut out);
    out
}

fn walk_snap(path: &Path, out: &mut BTreeMap<PathBuf, Snap>) {
    let meta = fs::symlink_metadata(path).unwrap();
    let ft = meta.file_type();
    let (kind, content) = if ft.is_symlink() {
        let t = fs::read_link(path).unwrap();
        ("symlink", t.to_string_lossy().into_owned().into_bytes())
    } else if ft.is_dir() {
        let mut names: Vec<String> = fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        ("dir", names.join("\n").into_bytes())
    } else if ft.is_file() {
        ("file", fs::read(path).unwrap())
    } else if ft.is_fifo() {
        ("fifo", Vec::new())
    } else if ft.is_socket() {
        ("socket", Vec::new())
    } else {
        ("other", Vec::new())
    };
    out.insert(
        path.to_path_buf(),
        Snap {
            kind: kind.to_string(),
            size: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            mode: meta.mode(),
            content,
        },
    );
    if ft.is_dir() {
        for e in fs::read_dir(path).unwrap() {
            walk_snap(&e.unwrap().path(), out);
        }
    }
}

/// Windows of 8 characters of every seeded secret, plus the full value.
fn leaked_indexes(text: &str) -> Vec<usize> {
    let mut leaked = Vec::new();
    for (i, s) in SECRETS.iter().enumerate() {
        let chars: Vec<char> = s.chars().collect();
        let mut hit = text.contains(s);
        for w in chars.windows(8) {
            let win: String = w.iter().collect();
            if text.contains(&win) {
                hit = true;
                break;
            }
        }
        if hit {
            leaked.push(i);
        }
    }
    leaked
}

fn assert_no_leak(label: &str, r: &Run) {
    let so = leaked_indexes(&r.stdout);
    let se = leaked_indexes(&r.stderr);
    assert!(
        so.is_empty(),
        "{label}: stdout leaked secret indexes {so:?}"
    );
    assert!(
        se.is_empty(),
        "{label}: stderr leaked secret indexes {se:?}"
    );
    // Stderr is a pipe here, so the progress line must never be written.
    assert!(
        !r.stderr.contains('\r') && !r.stderr.contains("scanned"),
        "{label}: progress output on a piped stderr"
    );
}

/// Restores permissions on drop, on every exit path.
struct RestoreMode {
    path: PathBuf,
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o755));
    }
}

fn rich() -> Env {
    let e = Env::new();
    // drifted stow unit and an in-sync one
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".zshrc", "export A=2\n");
    e.d("git/.gitconfig", "[user]\n  name = x\n");
    e.h(".gitconfig", "[user]\n  name = x\n");
    e.d(".dredgeignore", ".config/herdr/**\n");
    // committed secret in the dotfiles tree
    e.d("shell/.envrc", &format!("export AWS={}\n", SECRETS[2]));
    // untracked config with a secret, without, ignored, unknown
    e.h(
        ".config/ngrok/ngrok.yml",
        &format!("authtoken: {}\n", SECRETS[4]),
    );
    e.h(".config/herdr/config.toml", "theme = 1\n");
    e.h(".config/mystery/a.conf", "x = 1\n");
    // cache dir and state file
    e.h(".cache/thing/blob", "cached\n");
    e.h(".zsh_history", "ls\n");
    // a symlinked unit pointing at the dotfiles tree
    std::os::unix::fs::symlink(e.dots().join("git/.gitconfig"), e.home().join(".gitlink")).unwrap();
    e
}

#[test]
fn scan_is_read_only() {
    let e = rich();
    let parent = e.fx.root();
    let before = snapshot(&parent);
    assert!(before.len() > 15, "fixture unexpectedly small");

    let runs: &[&[&str]] = &[
        &[],
        &["--json"],
        &["--all"],
        &["--fail-on", "untracked"],
        &["--no-color", "--all", "--json"],
    ];
    for extra in runs {
        let r = e.scan(extra);
        assert!(
            matches!(r.code, Some(0) | Some(1)),
            "{extra:?}: {:?}",
            r.code
        );
        let after = snapshot(&parent);
        let changed: Vec<_> = before
            .iter()
            .filter(|(p, s)| after.get(*p) != Some(s))
            .map(|(p, _)| p.clone())
            .collect();
        let added: Vec<_> = after.keys().filter(|p| !before.contains_key(*p)).collect();
        assert!(changed.is_empty(), "{extra:?}: changed paths {changed:?}");
        assert!(added.is_empty(), "{extra:?}: new paths {added:?}");
    }
}

#[test]
fn seeded_secrets_never_in_stdout_or_stderr() {
    let e = Env::new();
    // untracked unit
    e.h(
        ".config/ngrok/ngrok.yml",
        &format!("authtoken: {}\n", SECRETS[4]),
    );
    e.h(
        ".ssh/id_ed25519",
        &format!("{}\n{}\n", SECRETS[0], SECRETS[1]),
    );
    e.h(".aws/credentials", &format!("key = {}\n", SECRETS[2]));
    e.h(".config/gh/hosts.yml", &format!("oauth: {}\n", SECRETS[3]));
    // dotfiles tree (committed secrets)
    e.d(
        "ssh/.ssh/id_ed25519",
        &format!("{}\n{}\n", SECRETS[0], SECRETS[1]),
    );
    e.d("aws/.aws/credentials", &format!("key = {}\n", SECRETS[2]));
    e.d(
        "gh/.config/gh/hosts.yml",
        &format!("oauth: {}\n", SECRETS[3]),
    );
    e.d(
        "ngrok/.config/ngrok/ngrok.yml",
        &format!("authtoken: {}\n", SECRETS[4]),
    );

    let runs: &[&[&str]] = &[
        &[],
        &["--json"],
        &["--all"],
        &["--no-color"],
        &["--json", "--all"],
        &["--fail-on", "untracked"],
    ];
    // Detection guard: the absence checks below prove nothing unless each
    // seeded family is actually detected, both untracked (home) and committed
    // (dotfiles). SECRETS[1] is a base64 key body that no rule matches: it is
    // a negative control and is only checked for absence.
    let r = e.scan(&["--json", "--all"]);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    let mut committed = Vec::new();
    if let Some(a) = v["committed_secrets"].as_array() {
        for f in a {
            committed.push((
                f["rule_id"].as_str().unwrap_or("").to_string(),
                f["path"].as_str().unwrap_or("").to_string(),
            ));
        }
    }
    let mut untracked = Vec::new();
    collect_rule_paths(&v["units"], &mut untracked);
    for rule in [
        "private-key",
        "aws-access-key-id",
        "github-token",
        "ngrok-authtoken",
    ] {
        assert!(
            committed
                .iter()
                .any(|(r, p)| r == rule && p.contains("/dots/")),
            "rule {rule} not detected in dotfiles tree"
        );
        assert!(
            untracked
                .iter()
                .any(|(r, p)| r == rule && p.contains("/home/")),
            "rule {rule} not detected in home"
        );
    }

    for extra in runs {
        let r = e.scan(extra);
        assert_no_leak(&format!("{extra:?}"), &r);
    }

    // error path
    let h = e.home_s();
    let missing =
        e.fx.root()
            .join("no-such-dots")
            .to_string_lossy()
            .into_owned();
    let r = scan(&["--home", &h, "--dotfiles", &missing]);
    assert_eq!(r.code, Some(2));
    assert_no_leak("error path", &r);
}

fn assert_one_line_error(r: &Run) {
    assert_eq!(r.code, Some(2), "stderr: {}", r.stderr);
    assert!(r.stdout.is_empty(), "stdout not empty: {}", r.stdout);
    assert!(r.stderr.starts_with("dredge:"), "stderr: {}", r.stderr);
    assert_eq!(
        r.stderr.trim_end_matches('\n').lines().count(),
        1,
        "{}",
        r.stderr
    );
}

#[test]
fn missing_dotfiles_dir_exits_two_with_one_line_error() {
    let e = Env::new();
    let h = e.home_s();
    let missing = e.fx.root().join("nope").to_string_lossy().into_owned();
    let r = scan(&["--home", &h, "--dotfiles", &missing]);
    assert_one_line_error(&r);
}

#[test]
fn dotfiles_is_a_file_exits_two() {
    let e = Env::new();
    let f = e.fx.file("dots-file");
    let h = e.home_s();
    let r = scan(&["--home", &h, "--dotfiles", &f.to_string_lossy()]);
    assert_one_line_error(&r);
}

#[test]
fn unreadable_directory_does_not_fail_scan() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIPPED: running as root, chmod 000 does not block reads");
        return;
    }
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".config/app/ok.conf", "a = 1\n");
    let locked = e.home().join(".config/app/locked");
    fs::create_dir_all(&locked).unwrap();
    write(&locked.join("hidden"), "h\n");
    let _guard = RestoreMode {
        path: locked.clone(),
    };
    e.fx.chmod(&locked, 0o000);

    let r = e.scan(&[]);
    assert!(
        matches!(r.code, Some(0) | Some(1)),
        "code {:?}: {}",
        r.code,
        r.stderr
    );
    let section = r
        .stdout
        .split("COULD NOT READ")
        .nth(1)
        .expect("COULD NOT READ section missing");
    assert!(
        section.contains("locked"),
        "locked dir not listed:\n{}",
        r.stdout
    );

    let r = e.scan(&["--json"]);
    assert!(matches!(r.code, Some(0) | Some(1)), "code {:?}", r.code);
    assert!(r.stdout.contains("locked"), "json lacks locked dir");
}

#[test]
fn unreadable_subdir_in_dotfiles_does_not_fail_scan() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIPPED: running as root, chmod 000 does not block reads");
        return;
    }
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".zshrc", "export A=2\n");
    e.d("git/.gitconfig", "[user]\n");
    e.h(".gitconfig", "[user]\n");
    e.d("locked/sub/.foo", "x\n");
    let locked = fs::canonicalize(e.dots()).unwrap().join("locked/sub");
    let _guard = RestoreMode {
        path: locked.clone(),
    };
    e.fx.chmod(&locked, 0o000);

    let r = e.scan(&[]);
    // Drift on .zshrc is still found, so the normal exit rules give 1.
    assert_eq!(r.code, Some(1), "stderr: {}", r.stderr);
    assert!(r.stderr.is_empty(), "stderr: {}", r.stderr);
    let section = r
        .stdout
        .split("COULD NOT READ")
        .nth(1)
        .expect("COULD NOT READ section missing");
    assert!(section.contains("dots/locked/sub"), "{}", r.stdout);
    assert!(
        r.stdout.contains("differs from dots/zsh/.zshrc"),
        "{}",
        r.stdout
    );

    let r = e.scan(&["--json", "--all"]);
    assert_eq!(r.code, Some(1));
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    let unreadable: Vec<&str> = v["unreadable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert!(
        unreadable.contains(&&*locked.to_string_lossy()),
        "{unreadable:?}"
    );
    let gitconfig = v["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["path"].as_str().unwrap().ends_with("/.gitconfig"))
        .expect("gitconfig unit");
    assert_eq!(gitconfig["tracking"], "tracked");
}

#[test]
fn fifo_in_home_does_not_hang() {
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".config/app/real.conf", "a = 1\n");
    e.fx.fifo("home/.config/app/pipe");
    e.fx.fifo("home/.fifo_unit");
    e.fx.fifo("home/.config/fifo_root_unit");
    let r = e.scan(&[]);
    assert!(
        matches!(r.code, Some(0) | Some(1)),
        "code {:?}: {}",
        r.code,
        r.stderr
    );
    let r = e.scan(&["--json", "--all"]);
    assert!(
        matches!(r.code, Some(0) | Some(1)),
        "code {:?}: {}",
        r.code,
        r.stderr
    );
}

#[test]
fn symlink_to_outside_not_followed() {
    let e = Env::new();
    let outside = e.fx.root().join("outside-zzz");
    fs::create_dir_all(&outside).unwrap();
    write(
        &outside.join("distinctive-hidden-name.conf"),
        &format!("authtoken: {}\n{}\n", SECRETS[4], SECRETS[3]),
    );
    write(
        &outside.join("sub/another-distinctive.key"),
        &format!("{}\n", SECRETS[0]),
    );
    std::os::unix::fs::symlink(&outside, e.home().join(".outside_link")).unwrap();
    e.h(".config/app/real.conf", "a = 1\n");
    std::os::unix::fs::symlink(&outside, e.home().join(".config/app/linked_cfg")).unwrap();
    std::os::unix::fs::symlink(&outside, e.home().join(".config/linked_root")).unwrap();
    e.d("zsh/.zshrc", "export A=1\n");

    for extra in [&[][..], &["--json"], &["--all"]] {
        let r = e.scan(extra);
        assert!(matches!(r.code, Some(0) | Some(1)), "code {:?}", r.code);
        let all = format!("{}{}", r.stdout, r.stderr);
        assert!(
            !all.contains("distinctive-hidden-name"),
            "{extra:?}: outside file name shown"
        );
        assert!(
            !all.contains("another-distinctive"),
            "{extra:?}: outside nested name shown"
        );
        assert_no_leak(&format!("{extra:?}"), &r);
        // The link itself is reported, not its contents.
        assert!(
            all.contains(".outside_link"),
            "{extra:?}: link not reported"
        );
        assert!(
            all.contains(".config/linked_root"),
            "{extra:?}: root link not reported"
        );
    }
    // Exact pipeline behaviour: root-level links are units flagged `symlink`;
    // a link nested in a unit is folded into that unit (which is not itself
    // a symlink) and never followed.
    let r = e.scan(&["--json", "--all"]);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    let units = v["units"].as_array().unwrap();
    let find = |suffix: &str| {
        units
            .iter()
            .find(|u| u["path"].as_str().is_some_and(|p| p.ends_with(suffix)))
            .unwrap_or_else(|| panic!("unit {suffix} missing"))
    };
    assert_eq!(find("/.outside_link")["symlink"], true);
    assert_eq!(find("/.config/linked_root")["symlink"], true);
    assert_eq!(find("/.config/app")["symlink"], false);
    for u in units {
        let p = u["path"].as_str().unwrap_or("");
        assert!(!p.contains("outside-zzz"), "outside path became a unit");
    }
}

#[test]
fn socket_in_home_is_skipped() {
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".config/app/real.conf", "a = 1\n");
    let sock_dir = e.home().join(".config/app");
    let _l1 = UnixListener::bind(sock_dir.join("agent.sock")).unwrap();
    let _l2 = UnixListener::bind(e.home().join(".root_socket")).unwrap();
    let r = e.scan(&[]);
    assert!(
        matches!(r.code, Some(0) | Some(1)),
        "code {:?}: {}",
        r.code,
        r.stderr
    );
    assert!(r.stderr.is_empty(), "stderr: {}", r.stderr);
    let r = e.scan(&["--json", "--all"]);
    assert!(
        matches!(r.code, Some(0) | Some(1)),
        "code {:?}: {}",
        r.code,
        r.stderr
    );
    assert!(r.stderr.is_empty(), "stderr: {}", r.stderr);
    assert!(!r.stdout.contains("agent.sock"), "socket reported");
    assert!(!r.stdout.contains(".root_socket"), "root socket reported");
    // Control: the sibling regular file's unit is listed.
    assert!(r.stdout.contains(".config/app"), "control unit missing");
    assert!(sock_dir.join("agent.sock").exists());
}

#[test]
fn huge_file_is_skipped_for_secret_scan() {
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    let mut big = format!("authtoken: {}\n", SECRETS[4]);
    big.push_str(&"#".repeat(1_048_576 + 10));
    e.h(".config/bigapp/big.conf", &big);
    // Control: the same content, small, is reported.
    e.h(
        ".config/smallapp/small.conf",
        &format!("authtoken: {}\n", SECRETS[4]),
    );

    let r = e.scan(&["--json", "--all"]);
    assert!(matches!(r.code, Some(0) | Some(1)), "code {:?}", r.code);
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    let text = v.to_string();
    assert!(text.contains("bigapp"), "big file unit not listed");
    // Find every object that carries a secret finding and check where it lives.
    let mut secret_paths = Vec::new();
    collect_secret_paths(&v, &mut secret_paths);
    assert!(
        secret_paths.iter().any(|p| p.contains("smallapp")),
        "control secret not detected: {secret_paths:?}"
    );
    assert!(
        !secret_paths.iter().any(|p| p.contains("bigapp")),
        "huge file was scanned: {secret_paths:?}"
    );
}

/// Collects (rule_id, path) from any JSON object that has a `rule_id`.
fn collect_rule_paths(v: &serde_json::Value, out: &mut Vec<(String, String)>) {
    match v {
        serde_json::Value::Object(m) => {
            if let (Some(r), Some(p)) = (
                m.get("rule_id").and_then(|x| x.as_str()),
                m.get("path").and_then(|x| x.as_str()),
            ) {
                out.push((r.to_string(), p.to_string()));
            }
            for c in m.values() {
                collect_rule_paths(c, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|c| collect_rule_paths(c, out)),
        _ => {}
    }
}

/// Collects `path` strings from any JSON object that also has a `rule_id`.
fn collect_secret_paths(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            if m.contains_key("rule_id") {
                if let Some(p) = m.get("path").and_then(|p| p.as_str()) {
                    out.push(p.to_string());
                }
            }
            for c in m.values() {
                collect_secret_paths(c, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|c| collect_secret_paths(c, out)),
        _ => {}
    }
}

#[test]
fn no_network_dependency_in_crate() {
    const DENY: &[&str] = &[
        "reqwest",
        "hyper",
        "ureq",
        "curl",
        "surf",
        "isahc",
        "attohttpc",
        "minreq",
        "h2",
        "tokio",
        "async-std",
        "openssl",
        "native-tls",
        "rustls",
        "http-client",
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ["Cargo.toml", "Cargo.lock"] {
        let text = fs::read_to_string(root.join(file)).unwrap();
        for line in text.lines() {
            let l = line.trim();
            // Cargo.toml: `name = ...` dependency keys; Cargo.lock: `name = "x"`.
            let name = if file == "Cargo.lock" {
                l.strip_prefix("name = \"")
                    .and_then(|r| r.strip_suffix('"'))
            } else {
                l.split(['=', ' ']).next().filter(|n| !n.is_empty())
            };
            if let Some(n) = name {
                assert!(!DENY.contains(&n), "{file} depends on network crate {n}");
            }
        }
    }
}

#[test]
fn pruned_cache_dirs_are_never_opened() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIPPED: running as root, chmod 000 does not block reads");
        return;
    }
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    let app = "Library/Application Support/Electronish";
    e.h(&format!("{app}/config.json"), "{}\n");
    // Mixed-case Chromium storage names; each holds something that would be
    // reported as unreadable (or as a secret) if it were listed or opened.
    let locked_file = e.h(&format!("{app}/CODE CACHE/js/locked.bin"), "x\n");
    let locked_dir = e.home().join(format!("{app}/indexeddb/https_x.leveldb"));
    fs::create_dir_all(&locked_dir).unwrap();
    write(&locked_dir.join("000003.log"), "x\n");
    e.h(
        &format!("{app}/local storage/leveldb/000005.ldb"),
        &format!("authtoken: {}\n", SECRETS[4]),
    );
    let fifo = e.home().join(format!("{app}/Service Worker/pipe"));
    fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let _g1 = RestoreMode {
        path: locked_file.clone(),
    };
    let _g2 = RestoreMode {
        path: locked_dir.clone(),
    };
    e.fx.chmod(&locked_file, 0o000);
    e.fx.chmod(&locked_dir, 0o000);

    for extra in [
        &["--json", "--all", "--jobs", "1"][..],
        &["--json", "--all", "--jobs", "4"],
    ] {
        let r = e.scan(extra);
        assert_eq!(r.code, Some(0), "code {:?}", r.code);
        assert_no_leak(&format!("{extra:?}"), &r);
        let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
        assert_eq!(v["unreadable"].as_array().map(Vec::len), Some(0));
        assert_eq!(v["pruned_dirs"], 4);
        let mut found = Vec::new();
        collect_rule_paths(&v["units"], &mut found);
        assert!(
            found.is_empty(),
            "findings inside pruned dirs: {}",
            found.len()
        );
        let unit = v["units"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| {
                u["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("Electronish"))
            })
            .unwrap();
        assert_eq!(unit["kind"], "unknown");
    }
}

#[test]
fn scan_is_read_only_with_parallel_workers() {
    let e = rich();
    let parent = e.fx.root();
    let before = snapshot(&parent);
    assert!(before.len() > 15, "fixture unexpectedly small");
    let runs: &[&[&str]] = &[&["--jobs", "4"], &["--jobs", "4", "--json", "--all"]];
    for extra in runs {
        let r = e.scan(extra);
        assert!(
            matches!(r.code, Some(0) | Some(1)),
            "{extra:?}: {:?}",
            r.code
        );
        assert_no_leak(&format!("{extra:?}"), &r);
        let after = snapshot(&parent);
        let changed: Vec<_> = before
            .iter()
            .filter(|(p, s)| after.get(*p) != Some(s))
            .map(|(p, _)| p.clone())
            .collect();
        let added: Vec<_> = after.keys().filter(|p| !before.contains_key(*p)).collect();
        assert!(changed.is_empty(), "{extra:?}: changed paths {changed:?}");
        assert!(added.is_empty(), "{extra:?}: new paths {added:?}");
    }
}
