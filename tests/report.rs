mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use std::io::Read as _;

use assert_cmd::Command as Bin;
use clap::Parser;
use common::Fixture;
use dredge::cli::{Cli, Command, ScanArgs};
use dredge::model::{Kind, Report, Tracking};
use dredge::report::{display_path, render_human, render_json};
use dredge::scan;

/// Seeded fake secrets. None of these may ever appear in any output.
const NGROK_VALUE: &str = "2abcDEFghiJKLmnoPQRstuVWxyz123456";
const GH_VALUE: &str = "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";
const AWS_VALUE: &str = "AKIAQWERTYUIOPASDFGH";

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
    fn args(&self, extra: &[&str]) -> ScanArgs {
        let mut v: Vec<String> = vec!["dredge".into(), "scan".into(), "--home".into()];
        v.push(self.home().to_string_lossy().into_owned());
        v.extend(extra.iter().map(|s| s.to_string()));
        let Command::Scan(args) = Cli::parse_from(v).command;
        args
    }
    fn with_dots(&self, extra: &[&str]) -> ScanArgs {
        let mut v = vec![
            "--dotfiles".to_string(),
            self.dots().to_string_lossy().into_owned(),
        ];
        v.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        self.args(&refs)
    }
    fn run(&self, args: &ScanArgs) -> Report {
        scan::run(args).unwrap()
    }
    fn bin(&self) -> Bin {
        let mut c = Bin::cargo_bin("dredge").unwrap();
        c.env_remove("NO_COLOR")
            .args(["scan", "--home"])
            .arg(self.home());
        c
    }
}

/// A home with one item in every section, against a stow repo.
fn rich() -> Env {
    let e = Env::new();
    // drifted
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".zshrc", "export A=2\n");
    // tracked, in sync
    e.d("git/.gitconfig", "[user]\n  name = x\n");
    e.h(".gitconfig", "[user]\n  name = x\n");
    // in repo, missing on this machine
    e.d("vim/.vimrc", "set nu\n");
    // committed secret, and a gitignored one that must not be reported
    e.d("shell/.envrc", &format!("export GITHUB_TOKEN={GH_VALUE}\n"));
    e.d(".gitignore", "local.env\n");
    e.d("shell/local.env", &format!("AWS={AWS_VALUE}\n"));
    // untracked config with a secret, and without
    e.h(
        ".config/ngrok/ngrok.yml",
        &format!("authtoken: {NGROK_VALUE}\n"),
    );
    e.h(".config/herdr/config.toml", "theme = 1\n");
    // unknown
    e.h(".config/mystery/a.conf", "x = 1\n");
    // cache and state
    e.h(".cache/thing/blob", "cached\n");
    e.h(".zsh_history", "ls\n");
    e
}

const HUMAN_SECTIONS: &[&str] = &[
    "COMMITTED SECRETS",
    "UNTRACKED SECRETS",
    "UNTRACKED CONFIG",
    "TRACKED, DRIFTED",
    "UNKNOWN (",
    "COULD NOT READ",
    "IGNORED",
];

fn assert_order(out: &str, sections: &[&str]) {
    let mut last = 0;
    for s in sections {
        let at = out[last..]
            .find(s)
            .unwrap_or_else(|| panic!("section {s} missing or out of order in:\n{out}"));
        last += at + s.len();
    }
}

#[test]
fn human_output_section_order() {
    let e = rich();
    let rep = e.run(&e.with_dots(&[]));
    let out = render_human(&rep, false, false);
    assert_order(&out, HUMAN_SECTIONS);
    assert!(out.contains("IN REPO, MISSING ON THIS MACHINE"), "{out}");
    assert_order(
        &out,
        &["IGNORED", "IN REPO, MISSING ON THIS MACHINE", "~/.vimrc"],
    );
    assert!(out.contains("differs from dots/zsh/.zshrc"), "{out}");
    insta::assert_snapshot!("human_rich", out);
}

#[test]
fn json_has_schema_version_and_same_findings() {
    let e = rich();
    let rep = e.run(&e.with_dots(&[]));
    let v: serde_json::Value = serde_json::from_str(&render_json(&rep)).unwrap();
    assert_eq!(v["schema_version"], 1);
    let units = v["units"].as_array().unwrap();
    assert_eq!(units.len(), rep.units.len());
    let json_findings: Vec<(String, u64)> = units
        .iter()
        .flat_map(|u| u["findings"].as_array().unwrap().iter())
        .map(|f| {
            (
                f["rule_id"].as_str().unwrap().to_string(),
                f["line"].as_u64().unwrap(),
            )
        })
        .collect();
    let model_findings: Vec<(String, u64)> = rep
        .units
        .iter()
        .flat_map(|u| u.findings.iter())
        .map(|f| (f.rule_id.clone(), f.line as u64))
        .collect();
    assert_eq!(json_findings, model_findings);
    assert_eq!(json_findings, vec![("ngrok-authtoken".to_string(), 1)]);
    let committed = v["committed_secrets"].as_array().unwrap();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0]["rule_id"], "github-token");
    assert!(committed[0]["path"]
        .as_str()
        .unwrap()
        .ends_with("shell/.envrc"));
    // Absolute, like every other path in schema 1.
    let vimrc = rep.home.join(".vimrc").to_string_lossy().into_owned();
    let missing = v["missing_on_machine"].as_array().unwrap();
    assert!(missing.iter().any(|m| *m == *vimrc), "{missing:?}");
    assert!(missing
        .iter()
        .all(|m| Path::new(m.as_str().unwrap()).is_absolute()));
    let home = rep.home.to_string_lossy().into_owned();
    let normalized = render_json(&rep).replace(&home, "<HOME>").replace(
        &rep.dotfiles.clone().unwrap().to_string_lossy().into_owned(),
        "<DOTS>",
    );
    insta::assert_snapshot!("json_rich", normalized);
}

#[test]
fn unknown_capped_at_twenty_with_count() {
    let e = Env::new();
    for i in 0..25 {
        e.h(&format!(".config/unk{i:02}/a.conf"), "x\n");
    }
    let rep = e.run(&e.args(&[]));
    let out = render_human(&rep, false, false);
    assert!(
        out.contains("UNKNOWN (25)  showing 20, run with --all to list"),
        "{out}"
    );
    assert!(out.contains("~/.config/unk19"), "{out}");
    assert!(!out.contains("~/.config/unk20"), "{out}");
    let all = render_human(&rep, false, true);
    assert!(all.contains("UNKNOWN (25)\n"), "{all}");
    assert!(all.contains("~/.config/unk24"), "{all}");
}

#[test]
fn all_flag_lists_cache_and_state() {
    let e = rich();
    let rep = e.run(&e.with_dots(&[]));
    let out = render_human(&rep, false, false);
    assert!(out.contains("2 cache/state items"), "{out}");
    assert!(!out.contains("~/.cache"), "{out}");
    assert!(!out.contains("~/.zsh_history"), "{out}");
    let all = render_human(&rep, false, true);
    assert!(all.contains("CACHE/STATE"), "{all}");
    assert!(all.contains("~/.cache"), "{all}");
    assert!(all.contains("~/.zsh_history"), "{all}");
    assert!(all.contains("TRACKED\n"), "{all}");
    assert!(all.contains("~/.gitconfig"), "{all}");
    // Every unit, cache and state too, stays in the report.
    assert!(rep.units.iter().any(|u| u.kind == Kind::Cache));
    assert!(rep.units.iter().any(|u| u.kind == Kind::State));
}

fn assert_no_secret(s: &str) {
    for v in [NGROK_VALUE, GH_VALUE, AWS_VALUE] {
        assert!(!s.contains(v), "secret value leaked");
        assert!(!s.contains(&v[..12]), "secret prefix leaked");
        assert!(!s.contains(&v[v.len() - 12..]), "secret suffix leaked");
    }
}

#[test]
fn output_never_contains_seeded_secret_value() {
    let e = rich();
    let rep = e.run(&e.with_dots(&[]));
    for all in [false, true] {
        for color in [false, true] {
            assert_no_secret(&render_human(&rep, color, all));
        }
    }
    assert_no_secret(&render_json(&rep));
    assert_no_secret(&format!("{rep:?}"));
    for json in [false, true] {
        let mut c = e.bin();
        c.arg("--dotfiles").arg(e.dots()).arg("--all");
        if json {
            c.arg("--json");
        }
        let out = c.output().unwrap();
        assert_no_secret(&String::from_utf8_lossy(&out.stdout));
        assert_no_secret(&String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn clean_machine_prints_all_none_sections() {
    let e = Env::new();
    let rep = e.run(&e.args(&[]));
    assert!(rep.dotfiles.is_none());
    let out = render_human(&rep, false, false);
    assert_order(&out, HUMAN_SECTIONS);
    assert!(!out.contains("MISSING ON THIS MACHINE"), "{out}");
    insta::assert_snapshot!("human_clean", out);
    e.bin().assert().code(0);
}

#[test]
fn stow_symlinked_zshrc_and_symlinked_config_dir_are_tracked() {
    let e = Env::new();
    let zshrc = e.d("zsh/.zshrc", "export A=1\n");
    e.d("ghostty/.config/ghostty/config", "font-size = 12\n");
    symlink(&zshrc, e.home().join(".zshrc")).unwrap();
    fs::create_dir_all(e.home().join(".config")).unwrap();
    symlink(
        e.dots().join("ghostty/.config/ghostty"),
        e.home().join(".config/ghostty"),
    )
    .unwrap();
    // A symlink pointing elsewhere is untracked.
    let elsewhere = e.fx.file("elsewhere/.inputrc");
    symlink(&elsewhere, e.home().join(".inputrc")).unwrap();
    // A nested symlink into the repo inside a walked unit counts for that unit.
    e.d("git/.config/git/ignore", "*.swp\n");
    e.h(".config/git/config", "[core]\n");
    symlink(
        e.dots().join("git/.config/git/ignore"),
        e.home().join(".config/git/ignore"),
    )
    .unwrap();

    let rep = e.run(&e.with_dots(&[]));
    let get = |rel: &str| {
        let p = rep.home.join(rel);
        rep.units
            .iter()
            .find(|u| u.path == p)
            .unwrap_or_else(|| panic!("no unit {rel}: {:#?}", rep.units))
    };
    assert_eq!(get(".zshrc").tracking, Tracking::Tracked);
    assert!(get(".zshrc").symlink);
    assert_eq!(get(".config/ghostty").tracking, Tracking::Tracked);
    assert_eq!(get(".inputrc").tracking, Tracking::Untracked);
    assert_eq!(get(".config/git").tracking, Tracking::PartiallyTracked);
    assert!(
        rep.missing_on_machine.is_empty(),
        "{:?}",
        rep.missing_on_machine
    );
}

#[test]
fn committed_secret_in_dotfiles_tree_is_reported_and_gitignored_file_is_not() {
    let e = Env::new();
    e.d("shell/.envrc", &format!("export GITHUB_TOKEN={GH_VALUE}\n"));
    e.d(".gitignore", "local.env\nignored-dir/\n");
    e.d("shell/local.env", &format!("AWS={AWS_VALUE}\n"));
    e.d("ignored-dir/x", &format!("AWS={AWS_VALUE}\n"));
    // .git is skipped even when it holds secrets
    e.d(".git/config", &format!("AWS={AWS_VALUE}\n"));
    // a symlink in the repo is not followed
    let outside = e.fx.root().join("outside.txt");
    write(&outside, &format!("AWS={AWS_VALUE}\n"));
    symlink(&outside, e.dots().join("link.txt")).unwrap();
    let rep = e.run(&e.with_dots(&[]));
    let got: Vec<(String, PathBuf, usize, bool)> = rep
        .committed_findings
        .iter()
        .map(|f| (f.rule_id.clone(), f.path.clone(), f.line, f.committed))
        .collect();
    let dots = fs::canonicalize(e.dots()).unwrap();
    assert_eq!(
        got,
        vec![(
            "github-token".to_string(),
            dots.join("shell/.envrc"),
            1,
            true
        )]
    );
    let out = render_human(&rep, false, false);
    assert!(out.contains("github-token"), "{out}");
    assert!(out.contains("dots/shell/.envrc"), "{out}");
    assert_no_secret(&out);
}

#[test]
fn secret_in_ignored_path_is_still_reported_and_exit_one() {
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    e.d(".dredgeignore", ".config/ngrok\n");
    e.h(
        ".config/ngrok/ngrok.yml",
        &format!("authtoken: {NGROK_VALUE}\n"),
    );
    let out = e.bin().arg("--dotfiles").arg(e.dots()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_order(
        &stdout,
        &["UNTRACKED SECRETS", "ngrok-authtoken", "UNTRACKED CONFIG"],
    );
    assert!(stdout.contains("~/.config/ngrok/ngrok.yml"), "{stdout}");
    assert!(stdout.contains("1 ignored by .dredgeignore"), "{stdout}");
    assert_no_secret(&stdout);
}

#[test]
fn cache_and_state_files_are_never_secret_scanned() {
    let e = Env::new();
    e.h(
        ".cache/tool/token.txt",
        &format!("authtoken: {NGROK_VALUE}\n"),
    );
    e.h(".zsh_history", &format!("export T={GH_VALUE}\n"));
    e.h(".foo/data.db", &format!("authtoken: {NGROK_VALUE}\n"));
    // cache and state files inside a config unit are not scanned either
    e.h(".claude/settings.json", "{}\n");
    e.h(".claude/cache/blob", &format!("authtoken: {NGROK_VALUE}\n"));
    e.h(
        ".claude/history.jsonl",
        &format!("authtoken: {NGROK_VALUE}\n"),
    );
    // nor a cache file inside an unknown unit
    e.h(".config/mystery/a.conf", "x = 1\n");
    e.h(
        ".config/mystery/node_modules/x",
        &format!("authtoken: {NGROK_VALUE}\n"),
    );
    let rep = e.run(&e.args(&[]));
    let claude = rep
        .units
        .iter()
        .find(|u| u.path == rep.home.join(".claude"))
        .unwrap();
    assert_eq!(claude.kind, Kind::Config);
    // Cache/state files inside mixed units are not counted in the summary.
    assert_eq!(rep.cache_state_count, 3, "{:#?}", rep.units);
    assert!(
        rep.units.iter().all(|u| u.findings.is_empty()),
        "{:#?}",
        rep.units
    );
    e.bin().assert().code(0);
}

#[test]
fn preferences_plist_is_listed_but_not_scanned() {
    let e = Env::new();
    let plist = e.h(
        "Library/Preferences/com.example.app.plist",
        &format!("<string>authtoken: {NGROK_VALUE}</string>\n"),
    );
    let rep = e.run(&e.args(&[]));
    let plist = fs::canonicalize(plist).unwrap();
    let unit = rep
        .units
        .iter()
        .find(|u| u.path == plist)
        .expect("plist listed");
    assert!(unit.findings.is_empty());
    let out = render_human(&rep, false, false);
    assert!(
        out.contains("~/Library/Preferences/com.example.app.plist"),
        "{out}"
    );
}

#[test]
fn json_is_valid_with_non_utf8_path_name() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let e = Env::new();
    let name = OsStr::from_bytes(b".bad\xffname");
    let created = fs::write(e.home().join(name), "x\n").is_ok();
    let rep = if created {
        eprintln!("branch: created a non-UTF-8 file name on disk");
        e.run(&e.args(&[]))
    } else {
        eprintln!("branch: filesystem refused the name, testing the lossy helper directly");
        Report {
            home: e.home(),
            units: Vec::new(),
            unreadable: vec![e.home().join(name)],
            ..Report::default()
        }
    };
    let json = render_json(&rep);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(v["schema_version"], 1);
    assert!(json.contains("\u{fffd}"), "{json}");
    // The display helper never fails on such a name either.
    let shown = display_path(&rep, &rep.home.join(name));
    assert!(shown.starts_with("~/.bad"), "{shown}");
    assert!(shown.contains('\u{fffd}'));
}

#[test]
fn missing_dotfiles_dir_is_error() {
    let e = Env::new();
    let args = e.with_dots(&[]);
    let err = scan::run(&args).unwrap_err();
    assert!(err.to_string().contains("--dotfiles"), "{err}");
    let out = e.bin().arg("--dotfiles").arg(e.dots()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&out.stderr).lines().count(), 1);
}

#[test]
fn chezmoi_default_source_dir_is_auto_detected() {
    let e = Env::new();
    e.h(".local/share/chezmoi/dot_zshrc", "export A=1\n");
    e.h(".zshrc", "export A=1\n");
    let rep = e.run(&e.args(&[]));
    let chez = fs::canonicalize(e.home().join(".local/share/chezmoi")).unwrap();
    assert_eq!(rep.dotfiles.as_deref(), Some(chez.as_path()));
    let zshrc = rep
        .units
        .iter()
        .find(|u| u.path == rep.home.join(".zshrc"))
        .unwrap();
    assert_eq!(zshrc.tracking, Tracking::Tracked);
    // The source dir itself is not reported as home content.
    assert!(rep
        .units
        .iter()
        .all(|u| u.findings.is_empty() && !u.path.starts_with(&chez)));
}

#[test]
fn exit_zero_when_only_untracked_config() {
    let e = Env::new();
    e.h(".config/herdr/config.toml", "theme = 1\n");
    e.bin().assert().code(0);
    e.bin().args(["--fail-on", "untracked"]).assert().code(1);
}

#[test]
fn exit_one_with_untracked_secret() {
    let e = Env::new();
    e.h(
        ".config/ngrok/ngrok.yml",
        &format!("authtoken: {NGROK_VALUE}\n"),
    );
    e.bin().assert().code(1);
    e.bin().args(["--fail-on", "untracked"]).assert().code(1);
}

#[test]
fn exit_one_with_drift_and_json_output() {
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".zshrc", "export A=2\n");
    let out = e
        .bin()
        .arg("--dotfiles")
        .arg(e.dots())
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
}

#[test]
fn non_tty_output_has_no_color_codes() {
    let e = rich();
    let out = e.bin().arg("--dotfiles").arg(e.dots()).output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains('\u{1b}'));
    let rep = e.run(&e.with_dots(&[]));
    assert!(render_human(&rep, true, false).contains('\u{1b}'));
}

#[test]
fn drift_in_partially_tracked_unit_is_listed_and_exits_one() {
    let e = Env::new();
    e.d("git/.config/git/config", "[core]\n  editor = vim\n");
    e.h(".config/git/config", "[core]\n  editor = nano\n");
    e.h(".config/git/extra", "[alias]\n");
    let rep = e.run(&e.with_dots(&[]));
    let unit = rep
        .units
        .iter()
        .find(|u| u.path == rep.home.join(".config/git"))
        .unwrap();
    assert_eq!(unit.tracking, Tracking::PartiallyTracked);
    let out = render_human(&rep, false, false);
    assert_order(
        &out,
        &[
            "TRACKED, DRIFTED",
            "~/.config/git/config",
            "differs from dots/git/.config/git/config",
            "UNKNOWN (",
        ],
    );
    e.bin().arg("--dotfiles").arg(e.dots()).assert().code(1);
}

fn json_unit_paths(stdout: &[u8]) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_slice(stdout).unwrap();
    v["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn extra_root_under_config_prints_one_note_and_keeps_units() {
    let e = Env::new();
    e.h(".config/atuin/config.toml", "x = 1\n");
    e.h(".config/fish/config.fish", "set x 1\n");
    e.h(".config/ngrok/ngrok.yml", "version: 2\n");
    e.h(".zshrc", "export A=1\n");
    let base = e.bin().arg("--json").output().unwrap();
    assert!(base.stderr.is_empty());
    let ngrok = e.home().join(".config/ngrok");
    let out = e
        .bin()
        .arg("--json")
        .arg("--root")
        .arg(&ngrok)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), base.status.code());
    assert_eq!(json_unit_paths(&out.stdout), json_unit_paths(&base.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(lines[0].starts_with("dredge: note: "), "{stderr}");
    assert!(lines[0].contains(&*ngrok.to_string_lossy()), "{stderr}");
    let config = fs::canonicalize(e.home().join(".config")).unwrap();
    assert!(
        lines[0].ends_with(&format!("covered by {}", config.display())),
        "{stderr}"
    );
}

#[test]
fn extra_root_inside_dotfiles_dir_prints_note() {
    let e = Env::new();
    e.d("zsh/.zshrc", "export A=1\n");
    e.h(".zshrc", "export A=1\n");
    let out = e
        .bin()
        .arg("--dotfiles")
        .arg(e.dots())
        .arg("--root")
        .arg(e.dots().join("zsh"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(lines[0].starts_with("dredge: note: "), "{stderr}");
    assert!(lines[0].contains("dotfiles directory"), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("dots/zsh"), "{stdout}");
}

#[test]
fn ignored_drifted_unit_still_fails_drift_end_to_end() {
    let e = Env::new();
    e.d("git/.gitconfig", "[user]\n  name = x\n");
    e.h(".gitconfig", "[user]\n  name = changed\n");
    e.d(".dredgeignore", ".gitconfig\n");
    let out = e
        .bin()
        .arg("--dotfiles")
        .arg(e.dots())
        .args(["--fail-on", "drift"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_order(
        &stdout,
        &[
            "TRACKED, DRIFTED",
            "~/.gitconfig",
            "differs from dots/git/.gitconfig",
            "UNKNOWN (",
        ],
    );
    assert!(stdout.contains("1 ignored by .dredgeignore"), "{stdout}");
    e.bin().arg("--dotfiles").arg(e.dots()).assert().code(1);
}

#[test]
fn unit_app_label_comes_from_an_entry_of_the_unit_kind() {
    let e = Env::new();
    // Unknown file decides the kind; the atuin state file must not name it.
    e.h(".local/bin/mytool", "#!/bin/sh\n");
    e.h(".local/share/atuin/history.db", "x\n");
    // A config unit still takes its config app name.
    e.h(".claude/history.jsonl", "{}\n");
    e.h(".claude/settings.json", "{}\n");
    let rep = e.run(&e.args(&[]));
    let get = |rel: &str| {
        let p = rep.home.join(rel);
        rep.units.iter().find(|u| u.path == p).unwrap()
    };
    let local = get(".local");
    assert_eq!(local.kind, Kind::Unknown);
    assert_eq!(local.app, None);
    let claude = get(".claude");
    assert_eq!(claude.kind, Kind::Config);
    assert_eq!(claude.app.as_deref(), Some("claude"));
    let out = render_human(&rep, false, false);
    assert!(!out.contains("atuin"), "{out}");
}

const BIG_LINE_FAKE: &str = "FAKE_TOKEN_9f8e7d6c5b4a3210";

fn big_secret_file(e: &Env, rel: &str) {
    let mut data = format!("authtoken: {BIG_LINE_FAKE}\n").into_bytes();
    data.resize(dredge::secrets::MAX_SCAN_BYTES as usize + 1, b'a');
    let p = e.home().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

#[test]
fn over_1mib_file_is_counted_not_scanned_and_sibling_is_reported() {
    let e = Env::new();
    big_secret_file(&e, ".config/big/huge.yml");
    e.h(
        ".config/big/small.yml",
        &format!("authtoken: {BIG_LINE_FAKE}\n"),
    );
    let rep = e.run(&e.args(&[]));
    assert_eq!(rep.skipped_too_large, 1);
    assert_eq!(rep.depth_truncated, 0);
    let findings: Vec<_> = rep.units.iter().flat_map(|u| u.findings.iter()).collect();
    assert_eq!(findings.len(), 1);
    assert!(findings[0].path.ends_with("small.yml"));
    // Exit code is not affected by the counters alone.
    let clean = Env::new();
    big_secret_file(&clean, ".config/big/huge.yml");
    clean.bin().assert().code(0);
}

#[test]
fn binary_and_symlink_skips_are_not_counted() {
    let e = Env::new();
    let mut bin = b"data".to_vec();
    bin.push(0);
    bin.resize(2_000, 1);
    let p = e.home().join(".config/b/blob.dat");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, bin).unwrap();
    symlink(&p, e.home().join(".config/b/link")).unwrap();
    let rep = e.run(&e.args(&[]));
    assert_eq!(rep.skipped_too_large, 0);
}

#[test]
fn depth_truncated_reaches_the_report() {
    let e = Env::new();
    e.h(".config/deep/a/b/c/d/e/x/y.txt", "x\n");
    let rep = e.run(&e.args(&[]));
    assert_eq!(rep.depth_truncated, 1);
}

#[test]
fn human_skip_lines_appear_only_when_counters_are_non_zero() {
    let rep = Report {
        home: PathBuf::from("/h"),
        depth_truncated: 3,
        skipped_too_large: 2,
        ..Report::default()
    };
    let out = render_human(&rep, false, false);
    assert!(
        out.contains("\n  3 directories deeper than 6 levels were not scanned\n"),
        "{out}"
    );
    assert!(
        out.contains("\n  2 files over 1 MiB were not scanned for secrets\n"),
        "{out}"
    );
    assert_order(&out, &["IGNORED", "directories deeper", "files over"]);

    let one = Report {
        home: PathBuf::from("/h"),
        depth_truncated: 1,
        skipped_too_large: 1,
        ..Report::default()
    };
    let out = render_human(&one, false, false);
    assert!(
        out.contains("  1 directory deeper than 6 levels was not scanned\n"),
        "{out}"
    );
    assert!(
        out.contains("  1 file over 1 MiB was not scanned for secrets\n"),
        "{out}"
    );

    let zero = render_human(
        &Report {
            home: PathBuf::from("/h"),
            ..Report::default()
        },
        false,
        false,
    );
    assert!(!zero.contains("deeper than"), "{zero}");
    assert!(!zero.contains("not scanned"), "{zero}");
}

#[test]
fn skip_lines_contain_only_numbers_and_fixed_text() {
    let e = Env::new();
    big_secret_file(&e, ".config/big/huge.yml");
    e.h(".config/deep/a/b/c/d/e/x/y.txt", "x\n");
    let rep = e.run(&e.args(&[]));
    assert_eq!((rep.depth_truncated, rep.skipped_too_large), (1, 1));
    let out = render_human(&rep, false, true);
    let lines: Vec<&str> = out.lines().filter(|l| l.contains("not scanned")).collect();
    assert_eq!(
        lines,
        vec![
            "  1 directory deeper than 6 levels was not scanned",
            "  1 file over 1 MiB was not scanned for secrets",
        ]
    );
    assert_no_secret(&out);
    assert!(!out.contains(BIG_LINE_FAKE));
    assert!(!render_json(&rep).contains(BIG_LINE_FAKE));
}

#[test]
fn json_has_skip_counters_always() {
    let clean = Env::new();
    let v: serde_json::Value =
        serde_json::from_str(&render_json(&clean.run(&clean.args(&[])))).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["depth_truncated_dirs"], 0);
    assert_eq!(v["skipped_too_large"], 0);

    let e = Env::new();
    big_secret_file(&e, ".config/big/huge.yml");
    e.h(".config/deep/a/b/c/d/e/x/y.txt", "x\n");
    let out = e.bin().arg("--json").output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["depth_truncated_dirs"], 1);
    assert_eq!(v["skipped_too_large"], 1);
}

#[test]
fn read_capped_flags_content_past_the_cap_even_when_lstat_was_small() {
    use dredge::secrets::{read_capped, MAX_SCAN_BYTES};
    // An endless reader stands in for a file that grew after its size check.
    let mut endless = std::io::repeat(b'a');
    let mut buf = Vec::new();
    assert!(read_capped(&mut endless, &mut buf).unwrap());
    assert_eq!(buf.len() as u64, MAX_SCAN_BYTES + 1);
    let mut exact = std::io::repeat(b'a').take(MAX_SCAN_BYTES);
    let mut buf = Vec::new();
    assert!(!read_capped(&mut exact, &mut buf).unwrap());
}

#[derive(Clone, Default)]
struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn progress_counts_processed_files_deterministically_and_clears() {
    use dredge::progress::Progress;
    let e = Env::new();
    e.h(".config/a/one.toml", "x = 1\n");
    e.h(".config/a/two.toml", "x = 2\n");
    e.h(".config/b/three.toml", "x = 3\n");
    let buf = SharedBuf::default();
    let mut p = Progress::new(Box::new(buf.clone()), true, std::time::Duration::ZERO);
    let rep = scan::run_with_progress(&e.args(&[]), &mut p).unwrap();
    assert_eq!(rep.units.len(), 2);
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("dredge: scanned 3 files..."), "{text:?}");
    assert!(!text.contains("scanned 4"), "{text:?}");
    assert!(text.ends_with('\r'), "{text:?}");
    assert!(!text.contains(".config") && !text.contains("one.toml"));
    // Nothing is shown when disabled.
    let off = SharedBuf::default();
    let mut p = Progress::new(Box::new(off.clone()), false, std::time::Duration::ZERO);
    scan::run_with_progress(&e.args(&[]), &mut p).unwrap();
    assert!(off.0.lock().unwrap().is_empty());
}
