mod common;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use common::Fixture;
use dredge::walk::{self, WalkResult, MAX_DEPTH};

fn unit<'a>(r: &'a WalkResult, path: &Path) -> &'a walk::UnitFiles {
    r.units
        .iter()
        .find(|u| u.path == path)
        .unwrap_or_else(|| panic!("unit {} not found", path.display()))
}

fn scan_with_timeout(roots: Vec<PathBuf>, allowed: Vec<PathBuf>) -> WalkResult {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(walk::scan(&roots, &allowed));
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("walk did not finish in time (hang)")
}

#[test]
fn units_are_first_component_under_root() {
    let fx = Fixture::new();
    fx.file("root/.ssh/config");
    fx.file("root/.ssh/keys/id");
    fx.file("root/.zshrc");
    let root = fx.root().join("root");
    let r = walk::scan(&[root.clone()], &[]);
    assert_eq!(r.units.len(), 2);
    let ssh = unit(&r, &root.join(".ssh"));
    assert_eq!(ssh.files.len(), 2);
    let zshrc = unit(&r, &root.join(".zshrc"));
    assert_eq!(zshrc.files, vec![root.join(".zshrc")]);
}

#[test]
fn default_roots_and_home_dot_filter() {
    let fx = Fixture::new();
    fx.file("h/.zshrc");
    fx.file("h/notes.txt");
    fx.file("h/.config/app/c.toml");
    fx.file("h/Library/Preferences/a.plist");
    let home = fx.root().join("h");
    let roots = walk::default_roots(&home);
    assert_eq!(roots[0], home);
    assert!(roots.contains(&home.join(".config")));
    assert!(roots.contains(&home.join("Library/Application Support")));
    assert!(roots.contains(&home.join("Library/Preferences")));
    let r = walk::scan_home(&home, &[]);
    assert!(r.units.iter().all(|u| u.path != home.join("notes.txt")));
    unit(&r, &home.join(".zshrc"));
    assert!(r.units.iter().all(|u| u.path != home.join(".config")));
    unit(&r, &home.join(".config/app"));
    unit(&r, &home.join("Library/Preferences/a.plist"));
}

#[test]
fn fifo_and_socket_are_skipped() {
    let fx = Fixture::new();
    fx.file("root/.u/real");
    fx.fifo("root/.u/pipe");
    fx.fifo("root/.fifo_unit");
    let sock = fx.root().join("root/.u/sock");
    let _l = UnixListener::bind(&sock).unwrap();
    let root = fx.root().join("root");
    let r = scan_with_timeout(vec![root.clone()], vec![]);
    let u = unit(&r, &root.join(".u"));
    assert_eq!(u.files, vec![root.join(".u/real")]);
    assert!(r.units.iter().all(|u| u.path != root.join(".fifo_unit")));
}

#[test]
fn symlink_loop_terminates() {
    let fx = Fixture::new();
    fx.file("root/.u/f");
    fx.symlink("root/.u/loop", &fx.root().join("root/.u"));
    let root = fx.root().join("root");
    let r = scan_with_timeout(vec![root.clone()], vec![]);
    assert_eq!(r.symlinks, vec![root.join(".u/loop")]);
    assert_eq!(unit(&r, &root.join(".u")).files, vec![root.join(".u/f")]);
}

#[test]
fn symlink_outside_roots_is_reported_not_followed() {
    let fx = Fixture::new();
    fx.file("outside/secret");
    fx.file("root/.u/f");
    fx.symlink("root/.u/esc", &fx.root().join("outside"));
    fx.symlink("root/.esc_unit", &fx.root().join("outside"));
    let root = fx.root().join("root");
    let r = walk::scan(&[root.clone()], &[]);
    assert!(r.symlinks.contains(&root.join(".u/esc")));
    assert!(r.symlinks.contains(&root.join(".esc_unit")));
    let all: Vec<_> = r.units.iter().flat_map(|u| u.files.iter()).collect();
    assert!(all.iter().all(|f| !f.ends_with("secret")));
}

#[test]
fn symlink_into_allowed_target_is_listed_as_symlink_for_tracking() {
    let fx = Fixture::new();
    fx.file("dotfiles/zshrc");
    fx.file("root/.u/f");
    let link = fx.symlink("root/.zshrc", &fx.root().join("dotfiles/zshrc"));
    let root = fx.root().join("root");
    let allowed = vec![fx.root().join("dotfiles")];
    let r = walk::scan(&[root.clone()], &allowed);
    assert_eq!(r.symlinks, vec![link.clone()]);
    assert!(r.units.iter().all(|u| u.path != link));
}

#[test]
fn unreadable_dir_goes_to_unreadable_not_error() {
    let fx = Fixture::new();
    fx.file("root/.u/ok");
    let locked = fx.dir("root/.u/locked");
    fx.file("root/.u/locked/hidden");
    fx.chmod(&locked, 0o000);
    let root = fx.root().join("root");
    let r = walk::scan(&[root.clone()], &[]);
    fx.chmod(&locked, 0o755);
    assert_eq!(r.unreadable, vec![locked]);
    assert_eq!(unit(&r, &root.join(".u")).files, vec![root.join(".u/ok")]);
}

#[test]
fn depth_cap_stops_at_six() {
    assert_eq!(MAX_DEPTH, 6);
    let fx = Fixture::new();
    fx.file("root/.u/a/b/c/d/e/six.txt");
    fx.file("root/.u/a/b/c/d/e/f/seven.txt");
    let root = fx.root().join("root");
    let r = walk::scan(&[root.clone()], &[]);
    let files = &unit(&r, &root.join(".u")).files;
    assert_eq!(files, &vec![root.join(".u/a/b/c/d/e/six.txt")]);
}

#[test]
fn non_utf8_file_name_is_walked() {
    let fx = Fixture::new();
    let dir = fx.dir("root/.u");
    let bad = dir.join(OsStr::from_bytes(b"bad\xFFname"));
    let root = fx.root().join("root");
    match std::fs::write(&bad, b"x") {
        Ok(()) => {
            eprintln!("non_utf8 branch: created 0xFF file name");
            let r = walk::scan(&[root.clone()], &[]);
            assert_eq!(unit(&r, &root.join(".u")).files, vec![bad]);
        }
        Err(e) if e.raw_os_error() == Some(92) => {
            eprintln!("non_utf8 branch: EILSEQ fallback (APFS rejects the name)");
            let lossy = walk::display_lossy(Path::new(OsStr::from_bytes(b"bad\xFFname")));
            assert!(lossy.contains('\u{FFFD}'));
            let combining = "e\u{301}.txt";
            fx.file(&format!("root/.u/{combining}"));
            let r = walk::scan(&[root.clone()], &[]);
            let files = &unit(&r, &root.join(".u")).files;
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].file_name(), Some(OsStr::new(combining)));
        }
        Err(e) => panic!("unexpected error creating fixture: {e}"),
    }
}

#[test]
fn home_that_is_symlink_resolves() {
    let fx = Fixture::new();
    fx.file("real/.zshrc");
    fx.file("real/.ssh/config");
    let link = fx.symlink("home", &fx.root().join("real"));
    let r = walk::scan_home(&link, &[]);
    assert_eq!(r.units.len(), 2);
    assert!(r.symlinks.is_empty());
    assert!(r.unreadable.is_empty());
    for u in &r.units {
        assert!(u.path.starts_with(fx.root().join("real")));
    }
}

#[test]
fn default_roots_never_enumerate_non_dot_home_children() {
    let fx = Fixture::new();
    fx.file("h/Documents/d.txt");
    fx.file("h/Desktop/d.txt");
    fx.file("h/Library/Caches/c");
    fx.file("h/Library/Application Support/app/s");
    fx.file("h/Library/Preferences/p.plist");
    fx.file("h/.zshrc");
    fx.file("h/.ssh/config");
    let home = fx.root().join("h");
    let r = walk::scan(&walk::default_roots(&home), &[]);
    let mut got: Vec<PathBuf> = r.units.iter().map(|u| u.path.clone()).collect();
    got.sort();
    let mut want = vec![
        home.join(".ssh"),
        home.join(".zshrc"),
        home.join("Library/Application Support/app"),
        home.join("Library/Preferences/p.plist"),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn symlinked_root_is_reported_not_walked() {
    let fx = Fixture::new();
    fx.file("dotfiles/ngrok/ngrok.yml");
    fx.file("h/.zshrc");
    fx.symlink("h/.config", &fx.root().join("dotfiles"));
    let home = fx.root().join("h");
    let allowed = vec![fx.root().join("dotfiles")];
    let r = walk::scan(&walk::default_roots(&home), &allowed);
    assert_eq!(r.symlinks, vec![home.join(".config")]);
    assert_eq!(r.units.len(), 1);
    assert!(r.units.iter().flat_map(|u| &u.files).all(|f| !f.starts_with(fx.root().join("dotfiles"))));
}
