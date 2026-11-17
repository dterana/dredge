mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use clap::Parser;
use common::Fixture;
use dredge::cli::{Cli, Command, FailOn};
use dredge::dredgeignore::IgnoreList;
use dredge::exit;
use dredge::model::{Drift, Finding, Kind, Report, Tracking, Unit};

fn unit(path: &str, tracking: Tracking, findings: Vec<Finding>) -> Unit {
    Unit {
        path: PathBuf::from(path),
        app: None,
        kind: Kind::Config,
        tracking,
        findings,
        ignored: false,
        symlink: false,
        drifted: Vec::new(),
    }
}

fn finding(path: &str) -> Finding {
    Finding {
        rule_id: "r".into(),
        path: PathBuf::from(path),
        line: 1,
        committed: false,
    }
}

fn report(units: Vec<Unit>) -> Report {
    Report {
        units,
        ..Report::default()
    }
}

fn default_fail_on() -> Vec<FailOn> {
    let cli = Cli::parse_from(["dredge", "scan"]);
    let Command::Scan(args) = cli.command;
    args.fail_on
}

fn list_from(contents: &[u8]) -> (Fixture, IgnoreList) {
    let fx = Fixture::new();
    std::fs::write(fx.root().join(".dredgeignore"), contents).unwrap();
    let list = IgnoreList::load(&fx.root());
    (fx, list)
}

#[test]
fn comments_and_blank_lines_ignored() {
    let (_fx, list) = list_from(b"# comment\n\n   \n  .config/foo  \n");
    assert!(list.is_ignored(Path::new(".config/foo")));
    assert!(!list.is_ignored(Path::new("# comment")));
    assert!(!list.is_ignored(Path::new(".config/bar")));
}

#[test]
fn missing_file_is_empty() {
    let fx = Fixture::new();
    assert!(!IgnoreList::load(&fx.root()).is_ignored(Path::new(".config/foo")));
}

#[test]
fn glob_matches_paths() {
    let (_fx, list) = list_from(b".config/foo/**\n*.log\n[invalid\n");
    assert!(list.is_ignored(Path::new(".config/foo")));
    assert!(list.is_ignored(Path::new(".config/foo/a/b")));
    assert!(list.is_ignored(Path::new(".cache/x.log")));
    assert!(!list.is_ignored(Path::new(".config/foobar")));
    let (_fx2, list2) = list_from(b".config/foo\n");
    assert!(list2.is_ignored(Path::new(".config/foo")));
    assert!(list2.is_ignored(Path::new(".config/foo/inner")));
}

#[test]
fn default_untracked_only_exits_zero() {
    let r = report(vec![unit(".foo", Tracking::Untracked, vec![])]);
    assert_eq!(exit::code(&r, &default_fail_on()), 0);
}

#[test]
fn drift_exits_one_by_default() {
    let r = report(vec![unit(".foo", Tracking::TrackedDrifted, vec![])]);
    assert_eq!(exit::code(&r, &default_fail_on()), 1);
}

#[test]
fn partially_tracked_unit_with_drift_fails_on_drift() {
    let mut u = unit(".config/git", Tracking::PartiallyTracked, vec![]);
    u.drifted.push(Drift {
        path: PathBuf::from(".config/git/config"),
        repo_path: PathBuf::from("git/.config/git/config"),
    });
    let r = report(vec![u]);
    assert_eq!(exit::code(&r, &[FailOn::Drift]), 1);
    assert_eq!(exit::code(&r, &default_fail_on()), 1);
    assert_eq!(exit::code(&r, &[FailOn::Secrets]), 0);
}

#[test]
fn secret_exits_one() {
    let r = report(vec![unit(
        ".foo",
        Tracking::Tracked,
        vec![finding(".foo/a")],
    )]);
    assert_eq!(exit::code(&r, &default_fail_on()), 1);
    let mut committed = Report::default();
    committed.committed_findings.push(finding("x"));
    assert_eq!(exit::code(&committed, &default_fail_on()), 1);
}

#[test]
fn fail_on_untracked_exits_one() {
    let r = report(vec![unit(".foo", Tracking::PartiallyTracked, vec![])]);
    assert_eq!(exit::code(&r, &[FailOn::Untracked]), 1);
}

#[test]
fn fail_on_replaces_default() {
    let r = report(vec![unit(".foo", Tracking::TrackedDrifted, vec![])]);
    assert_eq!(exit::code(&r, &[FailOn::Untracked]), 0);
}

#[test]
fn ignored_path_with_secret_still_reports_secret() {
    let home = Path::new("/home/u");
    let mut r = report(vec![unit(
        "/home/u/.config/foo",
        Tracking::Untracked,
        vec![finding(".config/foo/k")],
    )]);
    let (_fx, list) = list_from(b".config/foo\n");
    list.apply(&mut r, home);
    assert_eq!(r.units.len(), 1);
    assert_eq!(exit::code(&r, &default_fail_on()), 1);
    assert_eq!(exit::code(&r, &[FailOn::Untracked]), 0);
}

#[test]
fn ignore_removes_clean_unit_and_counts_it() {
    let home = Path::new("/home/u");
    let mut r = report(vec![
        unit("/home/u/.config/foo", Tracking::Untracked, vec![]),
        unit("/home/u/.config/bar", Tracking::Untracked, vec![]),
        unit("/elsewhere/.config/foo", Tracking::Untracked, vec![]),
    ]);
    let (_fx, list) = list_from(b".config/foo\n");
    list.apply(&mut r, home);
    assert_eq!(r.units.len(), 2);
    assert_eq!(r.ignored_by_file, 1);
    assert!(r.units.iter().all(|u| !u.ignored));
    assert_eq!(exit::code(&r, &[FailOn::Untracked]), 1);
}

#[test]
fn ignored_unit_with_findings_is_kept_and_counted() {
    let home = Path::new("/home/u");
    let mut r = report(vec![unit(
        "/home/u/.config/foo",
        Tracking::Untracked,
        vec![finding("k")],
    )]);
    let (_fx, list) = list_from(b".config/foo/**\n");
    list.apply(&mut r, home);
    assert_eq!(r.units.len(), 1);
    assert!(r.units[0].ignored);
    assert_eq!(r.ignored_by_file, 1);
}

#[test]
fn symlinked_dredgeignore_is_treated_as_empty() {
    let fx = Fixture::new();
    let target = fx.file("real_ignore");
    std::fs::write(&target, b".config/foo\n").unwrap();
    let dotfiles = fx.dir("dots");
    fx.symlink("dots/.dredgeignore", &target);
    assert!(!IgnoreList::load(&dotfiles).is_ignored(Path::new(".config/foo")));
}

#[test]
fn fifo_dredgeignore_does_not_hang() {
    let fx = Fixture::new();
    let dotfiles = fx.dir("dots");
    fx.fifo("dots/.dredgeignore");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let list = IgnoreList::load(&dotfiles);
        let _ = tx.send(list.is_ignored(Path::new(".config/foo")));
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(ignored) => assert!(!ignored),
        Err(_) => panic!("loading a FIFO .dredgeignore hung"),
    }
}

#[test]
fn oversized_dredgeignore_is_treated_as_empty() {
    let mut contents = b".config/foo\n".to_vec();
    contents.extend(std::iter::repeat(b'#').take(70 * 1024));
    let (_fx, list) = list_from(&contents);
    assert!(!list.is_ignored(Path::new(".config/foo")));
}

#[test]
fn ignored_drifted_unit_still_fails_drift() {
    let home = Path::new("/home/u");
    let drifted_file = unit("/home/u/.gitconfig", Tracking::TrackedDrifted, vec![]);
    let mut partial = unit("/home/u/.config/git", Tracking::PartiallyTracked, vec![]);
    partial.drifted.push(Drift {
        path: PathBuf::from("/home/u/.config/git/config"),
        repo_path: PathBuf::from("/dots/git/.config/git/config"),
    });
    let clean = unit("/home/u/.config/foo", Tracking::Untracked, vec![]);
    let mut r = report(vec![drifted_file, partial, clean]);
    let (_fx, list) = list_from(b".gitconfig\n.config/*\n");
    list.apply(&mut r, home);
    assert_eq!(r.units.len(), 2);
    assert!(r.units.iter().all(|u| u.ignored));
    assert_eq!(r.ignored_by_file, 3);
    assert_eq!(exit::code(&r, &[FailOn::Drift]), 1);
    assert_eq!(exit::code(&r, &default_fail_on()), 1);
    // Ignoring still removes them from the untracked category.
    assert_eq!(exit::code(&r, &[FailOn::Untracked]), 0);
}
