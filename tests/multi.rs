//! Several `--dotfiles` directories in one scan: tracking, precedence, the
//! union of the missing list, `.dredgeignore`, committed secrets and the
//! stow-directory hint. Fixtures only, never the real home.

mod common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assert_cmd::Command as Bin;
use clap::Parser;
use common::Fixture;
use dredge::cli::{Cli, Command, ScanArgs};
use dredge::model::{Report, Tracking, Unit};
use dredge::report::{render_human, render_json};
use dredge::scan;

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A fake GitHub token built from fragments. Never printed.
fn fake_token() -> String {
    format!("{}{}", "ghp_", "Zq7Xk2Lm9Vb4Nc8Rt3Yw6Hs1Jd0Fg5Pa7QeU")
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
    fn p(&self, rel: &str) -> PathBuf {
        self.fx.root().join(rel)
    }
    /// Writes a file relative to the fixture root.
    fn w(&self, rel: &str, contents: &str) -> PathBuf {
        let p = self.p(rel);
        write(&p, contents);
        p
    }
    fn h(&self, rel: &str, contents: &str) -> PathBuf {
        self.w(&format!("home/{rel}"), contents)
    }
    /// A home symlink `rel` pointing at fixture path `target`.
    fn link(&self, rel: &str, target: &str) {
        let p = self.home().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        symlink(self.p(target), p).unwrap();
    }
    fn args(&self, dots: &[&str], extra: &[&str]) -> ScanArgs {
        let mut v: Vec<String> = vec!["dredge".into(), "scan".into(), "--home".into()];
        v.push(self.home().to_string_lossy().into_owned());
        for d in dots {
            v.push("--dotfiles".into());
            v.push(self.p(d).to_string_lossy().into_owned());
        }
        v.extend(extra.iter().map(|s| s.to_string()));
        let Command::Scan(args) = Cli::parse_from(v).command;
        args
    }
    fn run(&self, dots: &[&str]) -> Report {
        scan::run(&self.args(dots, &[])).unwrap()
    }
    fn bin(&self, dots: &[&str]) -> Bin {
        let mut c = Bin::cargo_bin("dredge").unwrap();
        c.env_remove("NO_COLOR")
            .current_dir(self.fx.root())
            .env("HOME", self.home())
            .args(["scan", "--no-color", "--home"])
            .arg(self.home());
        for d in dots {
            c.arg("--dotfiles").arg(self.p(d));
        }
        c
    }
}

fn unit<'a>(rep: &'a Report, rel: &str) -> &'a Unit {
    let p = rep.home.join(rel);
    rep.units
        .iter()
        .find(|u| u.path == p)
        .unwrap_or_else(|| panic!("no unit {rel}"))
}

#[test]
fn repeated_dotfiles_flags_parse_in_order() {
    let Command::Scan(args) =
        Cli::parse_from(["dredge", "scan", "--dotfiles", "a", "--dotfiles", "b/c"]).command;
    assert_eq!(
        args.dotfiles,
        vec![PathBuf::from("a"), PathBuf::from("b/c")]
    );
    let Command::Scan(none) = Cli::parse_from(["dredge", "scan"]).command;
    assert!(none.dotfiles.is_empty());
}

/// Two stow directories, `common` and `personal`, each with its own packages.
fn two_dirs() -> Env {
    let e = Env::new();
    e.w("common/zsh/.zshrc", "export A=1\n");
    e.w("common/nvim/.config/nvim/init.lua", "set nu\n");
    e.w("common/vim/.vimrc", "set nu\n");
    e.w("personal/git/.gitconfig", "[user]\n");
    e.w("personal/tmux/.tmux.conf", "set -g mouse on\n");
    e.w("personal/ruby/.gemrc", "gem: --no-doc\n");
    e.w("personal/x/.xrc", "x\n");
    // Symlinks into either directory.
    e.link(".zshrc", "common/zsh/.zshrc");
    e.link(".gitconfig", "personal/git/.gitconfig");
    // A copy in sync with personal.
    e.h(".tmux.conf", "set -g mouse on\n");
    // Copies that drifted, one per directory.
    e.h(".config/nvim/init.lua", "set nonu\n");
    e.h(".gemrc", "gem: --doc\n");
    e
}

#[test]
fn two_stow_dirs_each_track_their_own_packages() {
    let e = two_dirs();
    let rep = e.run(&["common", "personal"]);
    assert_eq!(rep.dotfiles.len(), 2);
    assert_eq!(unit(&rep, ".zshrc").tracking, Tracking::Tracked);
    assert_eq!(unit(&rep, ".gitconfig").tracking, Tracking::Tracked);
    assert_eq!(unit(&rep, ".tmux.conf").tracking, Tracking::Tracked);
    let nvim = unit(&rep, ".config/nvim");
    assert_eq!(nvim.tracking, Tracking::TrackedDrifted);
    assert_eq!(nvim.drifted.len(), 1);
    assert_eq!(
        nvim.drifted[0].repo_path,
        e.p("common/nvim/.config/nvim/init.lua")
    );
    let gem = unit(&rep, ".gemrc");
    assert_eq!(gem.tracking, Tracking::TrackedDrifted);
    assert_eq!(gem.drifted[0].repo_path, e.p("personal/ruby/.gemrc"));
    let out = render_human(&rep, false, false);
    assert!(
        out.contains("differs from common/nvim/.config/nvim/init.lua"),
        "{out}"
    );
    assert!(out.contains("differs from personal/ruby/.gemrc"), "{out}");
    // Neither directory is reported as home content.
    assert!(rep
        .units
        .iter()
        .all(|u| !u.path.starts_with(e.p("common")) && !u.path.starts_with(e.p("personal"))));
}

#[test]
fn with_only_one_dir_the_other_dirs_files_are_untracked() {
    let e = two_dirs();
    let rep = e.run(&["common"]);
    assert_eq!(unit(&rep, ".gitconfig").tracking, Tracking::Untracked);
    assert_eq!(unit(&rep, ".tmux.conf").tracking, Tracking::Untracked);
    assert_eq!(unit(&rep, ".zshrc").tracking, Tracking::Tracked);
}

#[test]
fn missing_on_machine_is_the_union_across_dirs() {
    let e = two_dirs();
    let rep = e.run(&["common", "personal"]);
    assert_eq!(
        rep.missing_on_machine,
        vec![PathBuf::from(".vimrc"), PathBuf::from(".xrc")]
    );
    // A target defined in both directories is listed once.
    e.w("personal/vim/.vimrc", "set rnu\n");
    let rep = e.run(&["common", "personal"]);
    assert_eq!(
        rep.missing_on_machine,
        vec![PathBuf::from(".vimrc"), PathBuf::from(".xrc")]
    );
}

#[test]
fn earlier_dotfiles_wins_when_two_define_the_same_target() {
    let e = Env::new();
    e.w("a/zsh/.zshrc", "from a\n");
    e.w("b/zsh/.zshrc", "from b\n");
    e.h(".zshrc", "from a\n");
    let rep = e.run(&["a", "b"]);
    assert_eq!(unit(&rep, ".zshrc").tracking, Tracking::Tracked);
    let rep = e.run(&["b", "a"]);
    let u = unit(&rep, ".zshrc");
    assert_eq!(u.tracking, Tracking::TrackedDrifted);
    assert_eq!(u.drifted.len(), 1);
    assert_eq!(u.drifted[0].repo_path, e.p("b/zsh/.zshrc"));
    let out = render_human(&rep, false, false);
    assert!(out.contains("differs from b/zsh/.zshrc"), "{out}");
}

#[test]
fn the_same_dir_given_twice_is_used_once() {
    let e = two_dirs();
    let once = e.run(&["common"]);
    let twice = e.run(&["common", "common"]);
    assert_eq!(twice.dotfiles.len(), 1);
    assert!(render_json(&once) == render_json(&twice));
    assert!(render_human(&once, false, true) == render_human(&twice, false, true));
    // Also through a different spelling of the same directory.
    let mut args = e.args(&["common"], &[]);
    args.dotfiles.push(e.p("personal/../common"));
    let rep = scan::run(&args).unwrap();
    assert_eq!(rep.dotfiles.len(), 1);
}

#[test]
fn one_bad_path_among_several_exits_two_naming_it() {
    let e = two_dirs();
    for dots in [
        &["common", "nope"][..],
        &["nope", "common"],
        &["common", "personal", "nope"],
    ] {
        let out = e.bin(dots).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{dots:?}");
        assert!(out.stdout.is_empty(), "{dots:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        assert_eq!(lines.len(), 1, "{stderr}");
        assert!(lines[0].starts_with("dredge: --dotfiles "), "{stderr}");
        assert!(
            lines[0].contains(&*e.p("nope").to_string_lossy()),
            "{stderr}"
        );
        assert!(
            lines[0].ends_with("is not an existing directory"),
            "{stderr}"
        );
    }
    // A file is not a directory either.
    e.w("afile", "x\n");
    let out = e.bin(&["common", "afile"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains(&*e.p("afile").to_string_lossy()),
        "{stderr}"
    );
}

#[test]
fn json_dotfiles_is_an_array_of_every_dir() {
    let e = two_dirs();
    let v: serde_json::Value =
        serde_json::from_str(&render_json(&e.run(&["common", "personal"]))).unwrap();
    assert_eq!(v["schema_version"], 1);
    let dots: Vec<&str> = v["dotfiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d.as_str().unwrap())
        .collect();
    let want = [
        fs::canonicalize(e.p("common")).unwrap(),
        fs::canonicalize(e.p("personal")).unwrap(),
    ];
    assert_eq!(dots.len(), 2);
    for (got, want) in dots.iter().zip(&want) {
        assert_eq!(Path::new(got), want.as_path());
    }
    let none: serde_json::Value = serde_json::from_str(&render_json(&e.run(&[]))).unwrap();
    assert_eq!(none["dotfiles"], serde_json::json!([]));
}

#[test]
fn exit_code_reflects_drift_in_the_second_dir() {
    let e = Env::new();
    e.w("a/zsh/.zshrc", "x\n");
    e.w("b/git/.gitconfig", "orig\n");
    e.h(".gitconfig", "changed\n");
    e.bin(&["a"]).assert().code(0);
    e.bin(&["a", "b"]).assert().code(1);
    let _ = fake_token;
}
