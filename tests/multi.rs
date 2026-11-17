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
}

#[test]
fn dredgeignore_is_the_union_of_every_dirs_file() {
    let e = Env::new();
    e.w("a/zsh/.zshrc", "x\n");
    e.w("a/.dredgeignore", ".config/fromfirst/**\n");
    e.w("b/git/.gitconfig", "x\n");
    e.w(
        "b/.dredgeignore",
        "# only in the second file\n.config/fromsecond/**\n",
    );
    // A third directory with no .dredgeignore at all is fine.
    e.w("c/tmux/.tmux.conf", "x\n");
    e.h(".config/fromfirst/a.conf", "x = 1\n");
    e.h(".config/fromsecond/b.conf", "x = 1\n");
    e.h(".config/neither/c.conf", "x = 1\n");
    let rep = e.run(&["a", "b", "c"]);
    let listed = |rel: &str| rep.units.iter().any(|u| u.path == rep.home.join(rel));
    assert!(!listed(".config/fromfirst"));
    assert!(!listed(".config/fromsecond"));
    assert!(listed(".config/neither"));
    assert_eq!(rep.ignored_by_file, 2);
    // Each file only through its own directory being listed.
    let rep = e.run(&["a", "c"]);
    assert_eq!(rep.ignored_by_file, 1);
    assert!(rep
        .units
        .iter()
        .any(|u| u.path == rep.home.join(".config/fromsecond")));
}

#[test]
fn nested_dirs_report_a_committed_secret_once() {
    let e = Env::new();
    let token = fake_token();
    e.w(
        "repo/dotfiles-common/shell/.envrc",
        &format!("export GITHUB_TOKEN={token}\n"),
    );
    e.w("repo/dotfiles-common/zsh/.zshrc", "x\n");
    e.w("repo/Scripts/setup.sh", "echo hi\n");
    for dots in [
        &["repo", "repo/dotfiles-common"][..],
        &["repo/dotfiles-common", "repo"],
    ] {
        let rep = e.run(dots);
        assert_eq!(rep.dotfiles.len(), 2, "{dots:?}");
        assert_eq!(rep.committed_findings.len(), 1, "{dots:?}");
        assert_eq!(
            rep.committed_findings[0].path,
            e.p("repo/dotfiles-common/shell/.envrc")
        );
        let out = e.bin(dots).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.matches("SECRET  rule: github-token").count(), 1);
        assert!(!stdout.contains(&token[..12]), "secret prefix leaked");
        assert!(
            !stdout.contains(&token[token.len() - 12..]),
            "secret leaked"
        );
    }
}

#[test]
fn committed_secrets_are_scanned_in_every_listed_dir() {
    let e = Env::new();
    let token = fake_token();
    e.w("a/zsh/.zshrc", "x\n");
    e.w("b/shell/.envrc", &format!("export GITHUB_TOKEN={token}\n"));
    e.w("b/.gitignore", "local.env\n");
    e.w(
        "b/shell/local.env",
        &format!("export GITHUB_TOKEN={token}\n"),
    );
    assert!(e.run(&["a"]).committed_findings.is_empty());
    let rep = e.run(&["a", "b"]);
    assert_eq!(rep.committed_findings.len(), 1);
    assert_eq!(rep.committed_findings[0].path, e.p("b/shell/.envrc"));
    assert!(rep.committed_findings[0].committed);
    e.bin(&["a", "b"]).assert().code(1);
}

#[test]
fn every_listed_dir_inside_home_is_left_out_of_the_walk() {
    let e = Env::new();
    e.w("home/.config/dots-a/zsh/.zshrc", "x\n");
    e.w("home/.config/dots-b/git/.gitconfig", "x\n");
    e.h(".config/other/c.conf", "x = 1\n");
    let rep = e.run(&["home/.config/dots-a", "home/.config/dots-b"]);
    let a = e.home().join(".config/dots-a");
    let b = e.home().join(".config/dots-b");
    assert!(rep
        .units
        .iter()
        .all(|u| !u.path.starts_with(&a) && !u.path.starts_with(&b)));
    assert!(rep
        .units
        .iter()
        .any(|u| u.path == e.home().join(".config/other")));
    // A --root inside the second one gets the usual note.
    let out = e
        .bin(&["home/.config/dots-a", "home/.config/dots-b"])
        .arg("--root")
        .arg(b.join("git"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        lines[0].starts_with("dredge: note: skipping --root"),
        "{stderr}"
    );
    assert!(
        lines[0].contains("inside the dotfiles directory"),
        "{stderr}"
    );
    assert!(lines[0].contains(&*b.to_string_lossy()), "{stderr}");
}

// ---- the stow-directory hint ----

/// A synthetic repo shaped like a stow repo with several stow directories:
/// `dotfiles-common` plus one directory per profile, a `.profile-name` file
/// and other top-level folders that are not stow directories. Home links
/// into `dotfiles-common` and `dotfiles-personal`.
fn genesis_like() -> Env {
    let e = Env::new();
    let g = "genesis";
    let w = |rel: &str, c: &str| e.w(&format!("{g}/{rel}"), c);
    w(".profile-name", "personal\n");
    w(".github/workflows/ci.yml", "on: push\n");
    w("Scripts/setup.sh", "echo setup\n");
    w("Scripts/lib/util.sh", "echo util\n");
    w("brewfile-professional/Brewfile", "brew \"git\"\n");
    w("herdr-app-mods/patch.diff", "--- a\n");
    w("vscode-professional/settings.json", "{}\n");
    w("dotfiles-common/zsh/.zshrc", "export A=1\n");
    w("dotfiles-common/zsh/.zprofile", "export B=1\n");
    w("dotfiles-common/nvim/.config/nvim/init.lua", "set nu\n");
    w("dotfiles-common/git/.gitconfig", "[user]\n");
    w("dotfiles-common/starship/.config/starship.toml", "x = 1\n");
    w("dotfiles-personal/tmux/.tmux.conf", "set -g mouse on\n");
    w("dotfiles-personal/ruby/.gemrc", "gem: --no-doc\n");
    w("dotfiles-professional/aws/.aws/config", "[default]\n");
    w(
        "dotfiles-professional/kube/.kube/config",
        "apiVersion: v1\n",
    );
    // Finder leaves these everywhere; they do not make a folder a package.
    w("dotfiles-common/.DS_Store", "\0\0\0\x01Bud1");
    let c = format!("{g}/dotfiles-common");
    let p = format!("{g}/dotfiles-personal");
    e.link(".zshrc", &format!("{c}/zsh/.zshrc"));
    e.link(".zprofile", &format!("{c}/zsh/.zprofile"));
    e.link(".gitconfig", &format!("{c}/git/.gitconfig"));
    e.link(".config/nvim", &format!("{c}/nvim/.config/nvim"));
    e.link(
        ".config/starship.toml",
        &format!("{c}/starship/.config/starship.toml"),
    );
    e.link(".tmux.conf", &format!("{p}/tmux/.tmux.conf"));
    // A regular copy in sync with the personal profile.
    e.h(".gemrc", "gem: --no-doc\n");
    e
}

fn hint_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|l| l.contains("looks like a stow directory"))
        .collect()
}

#[test]
fn hint_names_each_stow_directory_inside_a_repo_root() {
    let e = genesis_like();
    let rep = e.run(&["genesis"]);
    let root = e.p("genesis");
    let hints: Vec<&String> = rep
        .notes
        .iter()
        .filter(|n| n.contains("looks like a stow directory"))
        .collect();
    assert_eq!(hints.len(), 3, "{hints:?}");
    for (hint, name) in hints.iter().zip([
        "dotfiles-common",
        "dotfiles-personal",
        "dotfiles-professional",
    ]) {
        let path = root.join(name).to_string_lossy().into_owned();
        assert!(
            hint.starts_with(&format!(
                "{path} looks like a stow directory (its subfolders look like packages)"
            )),
            "{hint}"
        );
        assert!(hint.ends_with(&format!("--dotfiles {path}")), "{hint}");
    }
    // Notes never reach the rendered report.
    let mut quiet = rep.clone();
    quiet.notes.clear();
    assert!(render_json(&rep) == render_json(&quiet));
    assert!(render_human(&rep, false, true) == render_human(&quiet, false, true));
}

#[test]
fn hint_stays_quiet_for_real_packages() {
    let e = Env::new();
    // Real packages: a dot entry at the top, one plain file, or a single
    // subdirectory with a dot entry.
    e.w("dots/nvim/.config/nvim/init.lua", "x\n");
    e.w("dots/zsh/.zshrc", "x\n");
    e.w("dots/brewfile-professional/Brewfile", "x\n");
    e.w("dots/single/sub/.x", "x\n");
    // Two subdirectories, but none with a dot entry.
    e.w("dots/Scripts/lib/a.sh", "x\n");
    e.w("dots/Scripts/bin/b.sh", "x\n");
    // Two package-like subdirectories, but a dot entry at its own top.
    e.w("dots/mixed/.mixedrc", "x\n");
    e.w("dots/mixed/a/.a", "x\n");
    e.w("dots/mixed/b/.b", "x\n");
    // Package-like subdirectories reached only through symlinks.
    e.w("elsewhere/a/.a", "x\n");
    e.w("elsewhere/b/.b", "x\n");
    fs::create_dir_all(e.p("dots/linked")).unwrap();
    symlink(e.p("elsewhere/a"), e.p("dots/linked/a")).unwrap();
    symlink(e.p("elsewhere/b"), e.p("dots/linked/b")).unwrap();
    // A symlinked top-level directory is not followed.
    symlink(e.p("elsewhere"), e.p("dots/stowlink")).unwrap();
    let rep = e.run(&["dots"]);
    assert!(rep.notes.is_empty(), "{:?}", rep.notes);
    let out = e.bin(&["dots"]).output().unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn hint_needs_two_package_like_subdirectories() {
    let e = Env::new();
    e.w("dots/sub/one/.a", "x\n");
    e.w("dots/sub/plain/file", "x\n");
    assert!(e.run(&["dots"]).notes.is_empty());
    e.w("dots/sub/two/.b", "x\n");
    assert_eq!(e.run(&["dots"]).notes.len(), 1);
    // A dot entry at the top of `sub` silences it again.
    e.w("dots/sub/.stow-local-ignore", "x\n");
    assert!(e.run(&["dots"]).notes.is_empty());
}

#[test]
fn hint_only_for_stow_layout() {
    let e = Env::new();
    e.w("dots/dot_zshrc", "x\n");
    e.w("dots/sub/one/.a", "x\n");
    e.w("dots/sub/two/.b", "x\n");
    // Auto picks chezmoi here, and chezmoi has no packages.
    assert!(e.run(&["dots"]).notes.is_empty());
    let forced = scan::run(&e.args(&["dots"], &["--layout", "stow"])).unwrap();
    assert_eq!(forced.notes.len(), 1);
}

#[test]
fn genesis_like_repo_root_versus_its_stow_directories() {
    let e = genesis_like();
    // The repo root: hint printed, report degraded.
    let root = e.bin(&["genesis"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&root.stderr);
    let hints = hint_lines(&stderr);
    assert_eq!(hints.len(), 3, "{stderr}");
    assert_eq!(stderr.lines().count(), 3, "{stderr}");
    for h in &hints {
        assert!(h.starts_with("dredge: note: "), "{stderr}");
    }
    for name in [
        "dotfiles-common",
        "dotfiles-personal",
        "dotfiles-professional",
    ] {
        let path = e.p("genesis").join(name).to_string_lossy().into_owned();
        assert!(
            hints
                .iter()
                .any(|h| h.ends_with(&format!("--dotfiles {path}"))),
            "{stderr}"
        );
    }
    let stdout = String::from_utf8_lossy(&root.stdout);
    assert!(
        stdout.contains("IN REPO, MISSING ON THIS MACHINE"),
        "{stdout}"
    );
    assert!(stdout.contains("~/zsh/.zshrc"), "{stdout}");
    assert!(stdout.contains("UNTRACKED CONFIG"), "{stdout}");

    // Its stow directories: no hint, correct tracking, nothing missing.
    let dirs = ["genesis/dotfiles-common", "genesis/dotfiles-personal"];
    let good = e.bin(dirs.as_slice()).output().unwrap();
    assert!(
        good.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&good.stderr)
    );
    assert_eq!(good.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&good.stdout);
    assert!(!stdout.contains("MISSING ON THIS MACHINE"), "{stdout}");
    let json = e.bin(dirs.as_slice()).arg("--json").output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["missing_on_machine"], serde_json::json!([]));
    let rep = e.run(&dirs);
    for rel in [
        ".zshrc",
        ".zprofile",
        ".gitconfig",
        ".config/nvim",
        ".config/starship.toml",
        ".tmux.conf",
        ".gemrc",
    ] {
        assert_eq!(unit(&rep, rel).tracking, Tracking::Tracked, "{rel}");
    }
    assert!(rep.missing_on_machine.is_empty());
    // The professional profile is simply not compared.
    let prof = e.run(&["genesis/dotfiles-common", "genesis/dotfiles-professional"]);
    assert_eq!(unit(&prof, ".tmux.conf").tracking, Tracking::Untracked);
    assert_eq!(
        prof.missing_on_machine,
        vec![PathBuf::from(".aws/config"), PathBuf::from(".kube/config")]
    );
}

#[test]
fn hint_reads_a_bounded_number_of_entries() {
    let e = Env::new();
    e.w("dots/big/one/.a", "x\n");
    e.w("dots/big/two/.b", "x\n");
    assert_eq!(e.run(&["dots"]).notes.len(), 1);
    // Past the per-directory entry cap the directory is left alone.
    for i in 0..1100 {
        e.w(&format!("dots/big/f{i:04}"), "x\n");
    }
    assert!(e.run(&["dots"]).notes.is_empty());
}

#[test]
fn two_dirs_report_is_the_same_for_jobs_1_and_8() {
    let e = genesis_like();
    // Some extra content in every section.
    let token = fake_token();
    e.w(
        "genesis/dotfiles-personal/shell/.envrc",
        &format!("export GITHUB_TOKEN={token}\n"),
    );
    e.h(".config/ngrok/ngrok.yml", "version: 2\n");
    for i in 0..30 {
        e.h(&format!(".config/unk{i:02}/a.conf"), "x = 1\n");
        e.w(&format!("genesis/dotfiles-common/m{i:02}/.m{i:02}"), "x\n");
    }
    e.h(".gemrc", "drifted\n");
    let dirs = ["genesis/dotfiles-common", "genesis/dotfiles-personal"];
    for extra in [&[][..], &["--json"], &["--all"], &["--json", "--all"]] {
        let run = |jobs: &str| {
            e.bin(dirs.as_slice())
                .args(["--jobs", jobs])
                .args(extra)
                .output()
                .unwrap()
        };
        let one = run("1");
        let eight = run("8");
        assert_eq!(one.status.code(), Some(1), "{extra:?}");
        assert!(one.stdout == eight.stdout, "{extra:?}: stdout differs");
        assert!(one.stderr == eight.stderr, "{extra:?}: stderr differs");
        assert_eq!(one.status.code(), eight.status.code());
        let text = String::from_utf8_lossy(&one.stdout);
        assert!(!text.contains(&token[..12]), "secret prefix leaked");
        assert!(text.contains("github-token"), "{extra:?}");
    }
}

// ---- committed secrets: the enclosing git work tree's .gitignore files ----

/// A git work tree `genesis` (with `.git` as a directory, or as a file when
/// `git_file`) whose root `.gitignore` hides local files in its stow dirs.
fn git_repo(git_file: bool) -> Env {
    let e = Env::new();
    if git_file {
        e.w(
            "genesis/.git",
            "gitdir: ../elsewhere/.git/worktrees/genesis\n",
        );
    } else {
        e.w("genesis/.git/HEAD", "ref: refs/heads/main\n");
    }
    e.w(
        "genesis/.gitignore",
        "**/local.env\n/dotfiles-personal/zsh/secret\n",
    );
    let token = format!("export GITHUB_TOKEN={}\n", fake_token());
    e.w("genesis/dotfiles-common/zsh/.zshrc", "x\n");
    e.w("genesis/dotfiles-personal/zsh/.zprofile", "x\n");
    e.w("genesis/dotfiles-personal/zsh/local.env", &token);
    e.w("genesis/dotfiles-personal/zsh/secret", &token);
    e
}

const STOW_ARGS: [&[&str]; 4] = [
    &["genesis"],
    &["genesis/dotfiles-common", "genesis/dotfiles-personal"],
    &["genesis", "genesis/dotfiles-personal"],
    &["genesis/dotfiles-personal", "genesis/dotfiles-common"],
];

#[test]
fn git_root_gitignore_applies_to_listed_stow_subdirs() {
    for git_file in [false, true] {
        let e = git_repo(git_file);
        for dots in STOW_ARGS {
            let rep = e.run(dots);
            assert!(
                rep.committed_findings.is_empty(),
                "{dots:?} git_file {git_file}: {} committed",
                rep.committed_findings.len()
            );
            let out = e.bin(dots).output().unwrap();
            assert_eq!(out.status.code(), Some(0), "{dots:?} git_file {git_file}");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(!stdout.contains("SECRET  rule"), "{dots:?}: {stdout}");
        }
        // A token that is not ignored is still reported, once.
        e.w(
            "genesis/dotfiles-personal/zsh/.envrc",
            &format!("export GITHUB_TOKEN={}\n", fake_token()),
        );
        for dots in STOW_ARGS {
            let rep = e.run(dots);
            assert_eq!(rep.committed_findings.len(), 1, "{dots:?}");
            assert_eq!(
                rep.committed_findings[0].path,
                e.p("genesis/dotfiles-personal/zsh/.envrc")
            );
            e.bin(dots).assert().code(1);
        }
    }
}

#[test]
fn gitignore_files_between_git_root_and_listed_dir_apply() {
    let e = git_repo(false);
    e.w("genesis/dotfiles-personal/.gitignore", "mid.env\n");
    e.w(
        "genesis/dotfiles-personal/zsh/mid.env",
        &format!("T={}\n", fake_token()),
    );
    for dots in [
        &["genesis/dotfiles-personal/zsh"][..],
        &["genesis/dotfiles-personal"],
        &["genesis"],
    ] {
        assert!(e.run(dots).committed_findings.is_empty(), "{dots:?}");
    }
}

#[test]
fn without_a_git_root_only_ignore_files_inside_the_dir_apply() {
    let e = git_repo(false);
    fs::remove_dir_all(e.p("genesis/.git")).unwrap();
    // The repo root .gitignore is outside the listed dirs: not read.
    let rep = e.run(&["genesis/dotfiles-common", "genesis/dotfiles-personal"]);
    assert_eq!(rep.committed_findings.len(), 2);
    // Listing the root itself still honors its own .gitignore.
    assert!(e.run(&["genesis"]).committed_findings.is_empty());
}

#[test]
fn no_ignore_file_above_the_git_root_is_read() {
    let e = git_repo(false);
    // Above the repo: would hide the token if it were read.
    e.w(".gitignore", "*.env\nabove.env\n");
    e.w(
        "genesis/dotfiles-personal/zsh/above.env",
        &format!("T={}\n", fake_token()),
    );
    for dots in STOW_ARGS {
        let rep = e.run(dots);
        assert_eq!(rep.committed_findings.len(), 1, "{dots:?}");
        assert_eq!(
            rep.committed_findings[0].path,
            e.p("genesis/dotfiles-personal/zsh/above.env")
        );
    }
}

#[test]
fn a_symlinked_dot_git_is_not_a_git_root() {
    let e = git_repo(false);
    fs::remove_dir_all(e.p("genesis/.git")).unwrap();
    fs::create_dir_all(e.p("realgit")).unwrap();
    symlink(e.p("realgit"), e.p("genesis/.git")).unwrap();
    let rep = e.run(&["genesis/dotfiles-personal"]);
    assert_eq!(rep.committed_findings.len(), 2);
}
