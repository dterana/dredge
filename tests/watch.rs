//! `--watch`: force a secret scan of a path the normal walk skips. Fixtures
//! are built at runtime and the fake token is assembled from fragments.

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command as Bin;
use common::Fixture;
use serde_json::Value;

fn token() -> String {
    format!("ghp_{}{}", "abcdefghijklmnopqrstuvwxyz", "ABCDEFGHIJ")
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
    fn secret(&self, abs: &Path) -> PathBuf {
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, format!("GITHUB_TOKEN={}\n", token())).unwrap();
        abs.to_path_buf()
    }
    fn run(&self, extra: &[&str], watch: &[&Path]) -> Output {
        let mut c = Bin::cargo_bin("dredge").unwrap();
        c.env_remove("NO_COLOR")
            .args(["scan", "--no-progress", "--home"])
            .arg(self.home())
            .args(extra);
        for w in watch {
            c.arg("--watch").arg(w);
        }
        c.output().unwrap()
    }
    fn json(&self, watch: &[&Path]) -> (Value, Output) {
        let out = self.run(&["--json"], watch);
        let v = serde_json::from_slice(&out.stdout).unwrap();
        (v, out)
    }
}

fn findings(v: &Value) -> Vec<String> {
    v["units"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|u| u["findings"].as_array().unwrap().iter())
        .map(|f| format!("{}:{}", f["path"].as_str().unwrap(), f["line"]))
        .collect()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn finds_a_secret_in_a_cache_named_dir_only_when_watched() {
    let e = Env::new();
    let file = e.secret(&e.home().join(".cache/tool/token.txt"));
    let (plain, out) = e.json(&[]);
    assert!(findings(&plain).is_empty());
    assert_eq!(out.status.code(), Some(0));

    let dir = e.home().join(".cache/tool");
    let (v, out) = e.json(&[&dir]);
    assert_eq!(findings(&v), vec![format!("{}:1", file.display())]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(v["schema_version"], 1);

    let human = e.run(&["--no-color"], &[&dir]);
    let text = String::from_utf8_lossy(&human.stdout).into_owned();
    assert!(text.contains("1 path force-scanned with --watch"), "{text}");
    assert!(!text.contains(&token()));
    let plain = e.run(&["--no-color"], &[]);
    let text = String::from_utf8_lossy(&plain.stdout).into_owned();
    assert!(!text.contains("force-scanned"), "{text}");
}

#[test]
fn watching_a_file_scans_it_directly() {
    let e = Env::new();
    let file = e.secret(&e.home().join(".cache/tool/token.txt"));
    let (v, _) = e.json(&[&file]);
    assert_eq!(findings(&v).len(), 1);
}

#[test]
fn finds_a_secret_deeper_than_the_depth_cap() {
    let e = Env::new();
    let deep = e.home().join(".config/app/a/b/c/d/e/f/g");
    let file = e.secret(&deep.join("tok.txt"));
    let (plain, _) = e.json(&[]);
    assert!(findings(&plain).is_empty());
    assert!(plain["depth_truncated_dirs"].as_u64().unwrap() > 0);

    let (v, out) = e.json(&[&deep]);
    assert_eq!(findings(&v), vec![format!("{}:1", file.display())]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn the_depth_cap_applies_from_the_watched_dir() {
    let e = Env::new();
    let base = e.home().join(".cache/w");
    e.secret(&base.join("a/b/c/d/e/f/g/tok.txt"));
    let (v, _) = e.json(&[&base]);
    assert!(findings(&v).is_empty());
    assert!(v["depth_truncated_dirs"].as_u64().unwrap() > 0);
}

#[test]
fn a_missing_path_is_a_note_and_changes_nothing() {
    let e = Env::new();
    let missing = e.home().join(".cache/nope");
    let (v, out) = e.json(&[&missing]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stderr(&out).contains(&format!(
        "dredge: note: --watch {} does not exist",
        missing.display()
    )));
    assert!(findings(&v).is_empty());
    let human = e.run(&[], &[&missing]);
    assert!(!String::from_utf8_lossy(&human.stdout).contains("force-scanned"));
}

#[test]
fn a_symlink_is_not_followed() {
    let e = Env::new();
    let real = e.secret(&e.home().join(".cache/real/token.txt"));
    let link = e.home().join(".cache/link");
    symlink(real.parent().unwrap(), &link).unwrap();
    let (v, out) = e.json(&[&link]);
    assert!(findings(&v).is_empty());
    assert_eq!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("is a symlink, not followed"));

    let flink = e.home().join(".cache/flink");
    symlink(&real, &flink).unwrap();
    let (v, _) = e.json(&[&flink]);
    assert!(findings(&v).is_empty());
}

#[test]
fn a_symlink_inside_a_watched_dir_is_not_followed() {
    let e = Env::new();
    let outside = e.secret(&e.fx.root().join("outside/token.txt"));
    let dir = e.home().join(".cache/w");
    fs::create_dir_all(&dir).unwrap();
    symlink(&outside, dir.join("link")).unwrap();
    let (v, _) = e.json(&[&dir]);
    assert!(findings(&v).is_empty());
}

#[test]
fn a_path_outside_home_and_roots_is_skipped() {
    let e = Env::new();
    let outside = e.fx.root().join("outside");
    e.secret(&outside.join("token.txt"));
    let (v, out) = e.json(&[&outside]);
    assert!(findings(&v).is_empty());
    assert_eq!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("is outside the home directory"));
}

#[test]
fn a_path_under_a_root_is_allowed() {
    let e = Env::new();
    let outside = e.fx.root().join("extra");
    let file = e.secret(&outside.join("cache/token.txt"));
    let root = outside.to_string_lossy().into_owned();
    let out = e.run(&["--json", "--root", &root], &[&outside.join("cache")]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(findings(&v), vec![format!("{}:1", file.display())]);
}

#[test]
fn a_file_the_normal_scan_covers_is_not_reported_twice() {
    let e = Env::new();
    let file = e.secret(&e.home().join(".config/app/token.txt"));
    let (plain, _) = e.json(&[]);
    assert_eq!(findings(&plain).len(), 1);
    let (v, _) = e.json(&[&file]);
    assert_eq!(findings(&v), findings(&plain));
    let dir = e.home().join(".config/app");
    let (v, _) = e.json(&[&dir, &file, &dir]);
    assert_eq!(findings(&v), findings(&plain));
    assert_eq!(v["units"], plain["units"]);
}

#[test]
fn a_path_in_no_unit_makes_the_unit_the_walker_would() {
    let e = Env::new();
    let file = e.secret(&e.home().join("Documents/notes/token.txt"));
    let dir = e.home().join("Documents");
    let (v, out) = e.json(&[&dir]);
    assert_eq!(out.status.code(), Some(1));
    let units = v["units"].as_array().unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0]["path"], dir.to_string_lossy().as_ref());
    assert_eq!(units[0]["tracking"], "untracked");
    assert_eq!(findings(&v), vec![format!("{}:1", file.display())]);
}

#[test]
fn output_is_identical_for_any_job_count() {
    let e = Env::new();
    for i in 0..12 {
        e.secret(&e.home().join(format!(".config/app{i}/token.txt")));
    }
    e.secret(&e.home().join(".cache/tool/token.txt"));
    e.secret(&e.home().join("Documents/token.txt"));
    let w1 = e.home().join(".cache/tool");
    let w2 = e.home().join("Documents");
    let run = |jobs: &str| {
        let out = e.run(&["--json", "--jobs", jobs], &[&w1, &w2]);
        (out.stdout, out.status.code())
    };
    let one = run("1");
    assert_eq!(one, run("8"));
    assert_eq!(one.1, Some(1));
}
