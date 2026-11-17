mod common;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use common::Fixture;
use dredge::walk::{self, Root, RootKind, WalkResult, MAX_DEPTH};

fn unit<'a>(r: &'a WalkResult, path: &Path) -> &'a walk::UnitFiles {
    r.units
        .iter()
        .find(|u| u.path == path)
        .unwrap_or_else(|| panic!("unit {} not found", path.display()))
}

fn scan_with_timeout(root: PathBuf) -> WalkResult {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(walk::scan(&[Root::extra(root)], &[]));
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
    let r = walk::scan(&[Root::extra(root.clone())], &[]);
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
    assert_eq!(roots[0], Root::home(home.clone()));
    // Only the home root is dot-filtered, and that is explicit.
    let homes: Vec<&Root> = roots.iter().filter(|r| r.kind == RootKind::Home).collect();
    assert_eq!(homes, vec![&roots[0]]);
    for sub in [
        ".config",
        "Library/Application Support",
        "Library/Preferences",
    ] {
        assert!(roots.contains(&Root::builtin(home.join(sub))), "{sub}");
    }
    let r = walk::scan_home(&home);
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
    let r = scan_with_timeout(root.clone());
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
    let r = scan_with_timeout(root.clone());
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
    let r = walk::scan(&[Root::extra(root.clone())], &[]);
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
    let r = walk::scan(&[Root::extra(root.clone())], &[]);
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
    let r = walk::scan(&[Root::extra(root.clone())], &[]);
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
    let r = walk::scan(&[Root::extra(root.clone())], &[]);
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
            let r = walk::scan(&[Root::extra(root.clone())], &[]);
            assert_eq!(unit(&r, &root.join(".u")).files, vec![bad]);
        }
        Err(e) if e.raw_os_error() == Some(92) => {
            eprintln!("non_utf8 branch: EILSEQ fallback (APFS rejects the name)");
            let lossy = walk::display_lossy(Path::new(OsStr::from_bytes(b"bad\xFFname")));
            assert!(lossy.contains('\u{FFFD}'));
            let combining = "e\u{301}.txt";
            fx.file(&format!("root/.u/{combining}"));
            let r = walk::scan(&[Root::extra(root.clone())], &[]);
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
    let r = walk::scan_home(&link);
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
    let r = walk::scan(&walk::default_roots(&home), &[]);
    assert_eq!(r.symlinks, vec![home.join(".config")]);
    assert_eq!(r.units.len(), 1);
    assert!(r
        .units
        .iter()
        .flat_map(|u| &u.files)
        .all(|f| !f.starts_with(fx.root().join("dotfiles"))));
}

fn unit_paths(r: &WalkResult) -> Vec<PathBuf> {
    r.units.iter().map(|u| u.path.clone()).collect()
}

/// Every walked file belongs to exactly one unit.
fn assert_no_double_walk(r: &WalkResult) {
    let mut files: Vec<&PathBuf> = r.units.iter().flat_map(|u| &u.files).collect();
    let total = files.len();
    files.sort();
    files.dedup();
    assert_eq!(files.len(), total, "a file was walked twice");
}

fn config_home(fx: &Fixture) -> PathBuf {
    for f in [
        "h/.zshrc",
        "h/.config/atuin/config.toml",
        "h/.config/fish/config.fish",
        "h/.config/gh/config.yml",
        "h/.config/ngrok/ngrok.yml",
        "h/.config/starship.toml",
        "h/.local/bin/mytool",
        "h/.local/share/atuin/history.db",
        "h/Library/Caches/thing/blob",
        "h/Library/Application Support/Code/settings.json",
        "h/Library/Preferences/a.plist",
    ] {
        fx.file(f);
    }
    fx.root().join("h")
}

#[test]
fn extra_root_under_config_keeps_config_units() {
    let fx = Fixture::new();
    let home = config_home(&fx);
    let base = walk::scan_home(&home);
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(home.join(".config/ngrok")));
    let r = walk::scan(&roots, &[]);
    assert_eq!(unit_paths(&r), unit_paths(&base));
    for app in ["atuin", "fish", "gh", "ngrok"] {
        unit(&r, &home.join(".config").join(app));
    }
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].root, home.join(".config/ngrok"));
    assert_eq!(r.skipped[0].covered_by, home.join(".config"));
    assert!(!r.skipped[0].dotfiles);
    assert!(base.skipped.is_empty());
    assert_no_double_walk(&r);
}

#[test]
fn ancestor_root_yields_children_without_double_walk() {
    let fx = Fixture::new();
    let home = config_home(&fx);
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(home.join("Library")));
    let r = walk::scan(&roots, &[]);
    assert!(r.skipped.is_empty());
    let caches = unit(&r, &home.join("Library/Caches"));
    assert_eq!(caches.files, vec![home.join("Library/Caches/thing/blob")]);
    unit(&r, &home.join("Library/Application Support/Code"));
    unit(&r, &home.join("Library/Preferences/a.plist"));
    let paths = unit_paths(&r);
    assert!(!paths.contains(&home.join("Library/Application Support")));
    assert!(!paths.contains(&home.join("Library/Preferences")));
    assert_no_double_walk(&r);
}

#[test]
fn ancestor_root_excludes_nested_root_subtree_at_any_depth() {
    let fx = Fixture::new();
    fx.file("r/a/own.txt");
    fx.file("r/a/b/deep/x.txt");
    let r_dir = fx.root().join("r");
    let nested = r_dir.join("a/b");
    let r = walk::scan(
        &[Root::extra(r_dir.clone()), Root::builtin(nested.clone())],
        &[],
    );
    assert_eq!(
        unit(&r, &r_dir.join("a")).files,
        vec![r_dir.join("a/own.txt")]
    );
    assert_eq!(
        unit(&r, &nested.join("deep")).files,
        vec![nested.join("deep/x.txt")]
    );
    assert_no_double_walk(&r);
}

#[test]
fn nested_extra_root_is_not_double_walked() {
    let fx = Fixture::new();
    let home = config_home(&fx);
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(home.join(".local/share")));
    let r = walk::scan(&roots, &[]);
    let paths = unit_paths(&r);
    assert!(paths
        .iter()
        .all(|p| !p.starts_with(home.join(".local/share"))));
    let local = unit(&r, &home.join(".local"));
    assert!(local
        .files
        .contains(&home.join(".local/share/atuin/history.db")));
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].covered_by, home);
    assert_no_double_walk(&r);
}

#[test]
fn duplicate_extra_root_is_skipped_once() {
    let fx = Fixture::new();
    let home = config_home(&fx);
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(home.join(".config")));
    let r = walk::scan(&roots, &[]);
    assert_eq!(unit_paths(&r), unit_paths(&walk::scan_home(&home)));
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].covered_by, home.join(".config"));
}

#[test]
fn extra_root_under_non_dot_home_child_is_walked_unfiltered() {
    let fx = Fixture::new();
    let home = config_home(&fx);
    fx.file("h/Documents/proj/notes.txt");
    fx.file("h/Documents/proj/.env");
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(home.join("Documents/proj")));
    let r = walk::scan(&roots, &[]);
    assert!(r.skipped.is_empty());
    unit(&r, &home.join("Documents/proj/notes.txt"));
    unit(&r, &home.join("Documents/proj/.env"));
    // The home root itself still never enumerates non-dot children.
    assert!(unit_paths(&r).iter().all(|p| *p != home.join("Documents")));
}

#[test]
fn extra_root_inside_excluded_dir_is_skipped_with_note() {
    let fx = Fixture::new();
    let home = config_home(&fx);
    fx.file("h/dots/zsh/.zshrc");
    let dots = home.join("dots");
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(dots.join("zsh")));
    let r = walk::scan(&roots, std::slice::from_ref(&dots));
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].covered_by, dots);
    assert!(r.skipped[0].dotfiles);
    assert!(r.units.iter().all(|u| !u.path.starts_with(&dots)));
}

#[test]
fn depth_truncated_counts_exact_unentered_dirs() {
    let fx = Fixture::new();
    // Level 6 directories under the unit: listed, not entered.
    fx.file("root/.u/a/b/c/d/e/keep.txt");
    fx.file("root/.u/a/b/c/d/e/f1/x.txt");
    fx.file("root/.u/a/b/c/d/e/f2/g/y.txt");
    fx.dir("root/.u/a/b/c/d/e/f3");
    // A symlinked dir and a file at level 6 are not counted.
    let outside = fx.dir("outside");
    fx.symlink("root/.u/a/b/c/d/e/link", &outside);
    // A shallower tree adds nothing.
    fx.file("root/.v/a/b/z.txt");
    let root = fx.root().join("root");
    let r = walk::scan(&[Root::extra(root.clone())], &[]);
    assert_eq!(r.depth_truncated, 3);
    let shallow = walk::scan(&[Root::extra(fx.root().join("root/.v"))], &[]);
    assert_eq!(shallow.depth_truncated, 0);
}

#[test]
fn same_outside_symlink_root_twice_is_listed_once_with_one_skip() {
    let fx = Fixture::new();
    let home = config_home(&fx);
    fx.file("elsewhere/a.conf");
    let link = fx.symlink("linked", &fx.root().join("elsewhere"));
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(link.clone()));
    roots.push(Root::extra(link.clone()));
    let r = walk::scan(&roots, &[]);
    assert_eq!(r.symlinks, vec![link.clone()]);
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].root, link);
    assert_eq!(r.skipped[0].covered_by, link);
    assert!(!r.skipped[0].dotfiles);
}

#[test]
fn extra_root_equal_to_a_symlinked_builtin_root_is_skipped() {
    let fx = Fixture::new();
    fx.file("h/.zshrc");
    fx.file("real/atuin/config.toml");
    let config = fx.symlink("h/.config", &fx.root().join("real"));
    let home = fx.root().join("h");
    let mut roots = walk::default_roots(&home);
    roots.push(Root::extra(config.clone()));
    let r = walk::scan(&roots, &[]);
    assert_eq!(r.symlinks, vec![config.clone()]);
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].covered_by, config);
}

// ---- pruning of cache/state directories ----

use dredge::classify::prunes_dir;
use dredge::rules::Rules;

/// Walks the default roots of `home` with the same prune predicate the scan
/// uses: `classify::prunes_dir` on the home-relative directory.
fn scan_pruned(home: &Path, rules: &Rules, extra: &[PathBuf]) -> WalkResult {
    let pred = |d: &Path| prunes_dir(d.strip_prefix(home).unwrap_or(d), rules);
    let opts = walk::WalkOptions { prune: Some(&pred) };
    let mut roots = walk::default_roots(home);
    roots.extend(extra.iter().cloned().map(Root::extra));
    walk::scan_with(&roots, &[], &opts)
}

fn all_files(r: &WalkResult) -> Vec<&PathBuf> {
    r.units.iter().flat_map(|u| &u.files).collect()
}

#[test]
fn chromium_storage_dirs_in_any_case_are_pruned() {
    let fx = Fixture::new();
    let app = "h/Library/Application Support/Notion";
    fx.file(&format!("{app}/config.json"));
    for d in [
        "CACHE",
        "code cache",
        "indexeddb",
        "Local Storage",
        "SERVICE WORKER",
        "Blob_Storage",
    ] {
        fx.file(&format!("{app}/{d}/sub/f.txt"));
    }
    let home = fx.root().join("h");
    let r = scan_pruned(&home, &Rules::embedded(), &[]);
    let notion = unit(&r, &home.join("Library/Application Support/Notion"));
    assert!(!notion.pruned);
    assert_eq!(
        notion.files,
        vec![home.join("Library/Application Support/Notion/config.json")]
    );
    assert_eq!(r.pruned_dirs, 6);
    assert_eq!(r.depth_truncated, 0);
    // Without a predicate the same tree is walked in full.
    let plain = walk::scan_home(&home);
    assert_eq!(all_files(&plain).len(), 7);
    assert_eq!(plain.pruned_dirs, 0);
}

#[test]
fn cache_name_match_is_case_insensitive() {
    let fx = Fixture::new();
    // Separate units: a case-insensitive file system cannot hold all three
    // spellings side by side.
    for (u, d) in [(".a", "cache"), (".b", "CACHE"), (".c", "Cache")] {
        fx.file(&format!("h/{u}/keep.txt"));
        fx.file(&format!("h/{u}/{d}/inner.txt"));
    }
    let home = fx.root().join("h");
    let r = scan_pruned(&home, &Rules::embedded(), &[]);
    assert_eq!(r.pruned_dirs, 3);
    for u in [".a", ".b", ".c"] {
        assert_eq!(
            unit(&r, &home.join(u)).files,
            vec![home.join(u).join("keep.txt")]
        );
    }
}

#[test]
fn pruned_dirs_counts_only_the_outermost_directory() {
    let fx = Fixture::new();
    fx.file("h/.app/settings.toml");
    fx.file("h/.app/Cache/Code Cache/x");
    fx.file("h/.app/Cache/y");
    fx.file("h/.app/sub/IndexedDB/blob_storage/z");
    fx.file("h/.app/logs/tmp/w");
    // A unit root that is itself a cache dir.
    fx.file("h/.cache/tool/blob");
    // A rule-based state/cache dir (npm), not cache-named.
    fx.file("h/.npm/_cacache/index/k");
    fx.file("h/.npm/_update-notifier-last-checked");
    let home = fx.root().join("h");
    let rules = Rules::embedded();
    let r = scan_pruned(&home, &rules, &[]);
    // Cache, IndexedDB, logs, .cache and .npm/_cacache.
    assert_eq!(r.pruned_dirs, 5);
    assert_eq!(
        unit(&r, &home.join(".app")).files,
        vec![home.join(".app/settings.toml")]
    );
    let cache = unit(&r, &home.join(".cache"));
    assert!(cache.pruned);
    assert!(cache.files.is_empty());
    assert_eq!(
        unit(&r, &home.join(".npm")).files,
        vec![home.join(".npm/_update-notifier-last-checked")]
    );
    // An overlapping extra root does not count anything twice.
    let again = scan_pruned(&home, &rules, &[home.join(".app")]);
    assert_eq!(again.skipped.len(), 1);
    assert_eq!(again.pruned_dirs, 5);
}

#[test]
fn config_rule_beats_a_cache_looking_name() {
    let fx = Fixture::new();
    fx.file("h/.foo/Cache/settings.json");
    fx.file("h/.bar/Cache/blob");
    let home = fx.root().join("h");
    let rules = Rules::parse(
        "[[rule]]\napp=\"foo\"\nkind=\"config\"\nglobs=[\".foo/Cache/**\"]\ndescription=\"d\"\n",
    )
    .unwrap();
    let r = scan_pruned(&home, &rules, &[]);
    assert_eq!(
        unit(&r, &home.join(".foo")).files,
        vec![home.join(".foo/Cache/settings.json")]
    );
    assert!(unit(&r, &home.join(".bar")).files.is_empty());
    assert_eq!(r.pruned_dirs, 1);
}

#[test]
fn state_rule_prunes_a_plain_name() {
    let fx = Fixture::new();
    fx.file("h/.foo/keep.conf");
    fx.file("h/.foo/history/h1");
    let home = fx.root().join("h");
    let rules = Rules::parse(
        "[[rule]]\napp=\"foo\"\nkind=\"state\"\nglobs=[\".foo/history/**\"]\ndescription=\"d\"\n",
    )
    .unwrap();
    let r = scan_pruned(&home, &rules, &[]);
    assert_eq!(
        unit(&r, &home.join(".foo")).files,
        vec![home.join(".foo/keep.conf")]
    );
    assert_eq!(r.pruned_dirs, 1);
}

#[test]
fn pruned_dir_at_the_depth_cap_is_not_depth_truncated() {
    let fx = Fixture::new();
    // `.u/a/b/c/d/e` is depth 5; its children sit at the depth cap.
    fx.file("root/.u/a/b/c/d/e/Cache/x");
    fx.file("root/.u/a/b/c/d/e/deeper/y");
    let root = fx.root().join("root");
    let pred = |d: &Path| prunes_dir(d.strip_prefix(&root).unwrap_or(d), &Rules::embedded());
    let opts = walk::WalkOptions { prune: Some(&pred) };
    let r = walk::scan_with(&[Root::extra(root.clone())], &[], &opts);
    assert_eq!(r.pruned_dirs, 1);
    assert_eq!(r.depth_truncated, 1);
    let plain = walk::scan(&[Root::extra(root)], &[]);
    assert_eq!(plain.depth_truncated, 2);
}

#[test]
fn extra_root_inside_a_pruned_dir_is_walked() {
    let fx = Fixture::new();
    fx.file("h/.config/app/settings.json");
    fx.file("h/.config/app/Cache/keep/k.txt");
    let home = fx.root().join("h");
    let extra = home.join(".config/app/Cache/keep");
    let r = scan_pruned(&home, &Rules::embedded(), std::slice::from_ref(&extra));
    assert!(r.skipped.is_empty(), "{:?}", r.skipped);
    assert_eq!(r.pruned_dirs, 1);
    unit(&r, &extra.join("k.txt"));
}
