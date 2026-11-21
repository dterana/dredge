mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::Fixture;
use dredge::cli::LayoutArg;
use dredge::layout::{detect, Index, Layout};
use dredge::model::Tracking;
use dredge::track::{decide, missing_on_machine, unit_tracking};

fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

struct Restore(PathBuf);
impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o644));
    }
}

#[test]
fn auto_detects_chezmoi_by_dot_prefix_and_stow_otherwise() {
    let f = Fixture::new();
    let stow = f.dir("stowrepo");
    write(&stow.join("zsh/.zshrc"), "a");
    assert_eq!(detect(&stow, LayoutArg::Auto), Layout::Stow);

    let c1 = f.dir("c1");
    write(&c1.join("dot_zshrc"), "a");
    assert_eq!(detect(&c1, LayoutArg::Auto), Layout::Chezmoi);
    let c2 = f.dir("c2");
    write(&c2.join(".chezmoiignore"), "");
    assert_eq!(detect(&c2, LayoutArg::Auto), Layout::Chezmoi);
    let c3 = f.dir("c3");
    write(&c3.join(".chezmoiroot"), "home");
    assert_eq!(detect(&c3, LayoutArg::Auto), Layout::Chezmoi);

    assert_eq!(detect(&c1, LayoutArg::Stow), Layout::Stow);
    assert_eq!(detect(&stow, LayoutArg::Chezmoi), Layout::Chezmoi);
}

#[test]
fn stow_symlink_into_repo_is_tracked_without_reading() {
    let f = Fixture::new();
    let repo = f.dir("dotfiles");
    let target = repo.join("zsh/.zshrc");
    write(&target, "secret");
    write(&repo.join("zsh/.zprofile"), "p");
    let home = f.dir("home");
    let link = f.symlink("home/.zshrc", &target);
    // symlink to a different file inside the repo is still tracked
    let link2 = f.symlink("home/.other", &repo.join("zsh/.zprofile"));
    let outside = f.file("elsewhere");
    let link3 = f.symlink("home/.out", &outside);
    let index = Index::build(&repo, Layout::Stow).unwrap();

    // Prove the target is never opened: make it unreadable.
    f.chmod(&target, 0o000);
    let _guard = Restore(target.clone());
    assert!(fs::read(&target).is_err());
    assert_eq!(
        decide(&link, Path::new(".zshrc"), &repo, &index),
        Tracking::Tracked
    );
    assert_eq!(
        decide(&link2, Path::new(".other"), &repo, &index),
        Tracking::Tracked
    );
    assert_eq!(
        decide(&link3, Path::new(".out"), &repo, &index),
        Tracking::Untracked
    );
    let _ = home;
}

#[test]
fn stow_copy_identical_is_tracked_and_changed_is_drifted() {
    let f = Fixture::new();
    let repo = f.dir("dotfiles");
    write(&repo.join("zsh/.zshrc"), "same");
    write(&repo.join("git/.gitconfig"), "orig");
    write(&repo.join("ghostty/.config/ghostty/config"), "g");
    f.dir("home");
    let a = f.root().join("home/.zshrc");
    write(&a, "same");
    let b = f.root().join("home/.gitconfig");
    write(&b, "changed");
    let c = f.root().join("home/.config/ghostty/config");
    write(&c, "g");
    let d = f.root().join("home/.unknown");
    write(&d, "u");
    let index = Index::build(&repo, Layout::Stow).unwrap();
    assert_eq!(
        decide(&a, Path::new(".zshrc"), &repo, &index),
        Tracking::Tracked
    );
    assert_eq!(
        decide(&b, Path::new(".gitconfig"), &repo, &index),
        Tracking::TrackedDrifted
    );
    assert_eq!(
        decide(&c, Path::new(".config/ghostty/config"), &repo, &index),
        Tracking::Tracked
    );
    assert_eq!(
        decide(&d, Path::new(".unknown"), &repo, &index),
        Tracking::Untracked
    );
}

#[test]
fn chezmoi_dot_private_executable_prefixes_map_to_targets() {
    let f = Fixture::new();
    let repo = f.dir("cz");
    write(&repo.join("dot_zshrc"), "a");
    write(&repo.join("private_dot_ssh/config"), "b");
    write(&repo.join("dot_config/ghostty/config"), "c");
    write(&repo.join("dot_local/bin/executable_tool"), "d");
    write(&repo.join("readonly_dot_rc"), "e");
    write(&repo.join("dot_link"), "f");
    write(&repo.join("symlink_dot_ln"), "/somewhere");
    write(&repo.join("run_once_setup.sh"), "x");
    write(&repo.join(".chezmoiignore"), "x");
    write(&repo.join(".git/config"), "x");
    let index = Index::build(&repo, Layout::Chezmoi).unwrap();
    for (rel, src) in [
        (".zshrc", "dot_zshrc"),
        (".ssh/config", "private_dot_ssh/config"),
        (".config/ghostty/config", "dot_config/ghostty/config"),
        (".local/bin/tool", "dot_local/bin/executable_tool"),
        (".rc", "readonly_dot_rc"),
    ] {
        let e = index
            .lookup(Path::new(rel))
            .unwrap_or_else(|| panic!("missing {rel}"));
        assert_eq!(e.repo_path, repo.join(src));
        assert!(!e.unverified && !e.template, "{rel}");
    }
    assert!(index.lookup(Path::new(".ln")).unwrap().unverified);
    assert_eq!(index.entries().count(), 7);
}

#[test]
fn chezmoi_exact_prefix_is_unverified() {
    let f = Fixture::new();
    let repo = f.dir("cz");
    write(&repo.join("exact_dot_config/app/conf"), "a");
    write(&repo.join("dot_x/encrypted_key"), "b");
    write(&repo.join("modify_dot_y"), "c");
    f.dir("home");
    let h = f.root().join("home/.config/app/conf");
    write(&h, "a");
    let index = Index::build(&repo, Layout::Chezmoi).unwrap();
    let e = index.lookup(Path::new(".config/app/conf")).unwrap();
    assert!(e.unverified);
    assert!(index.lookup(Path::new(".x/key")).unwrap().unverified);
    assert!(index.lookup(Path::new(".y")).is_none());
    assert_eq!(
        decide(&h, Path::new(".config/app/conf"), &repo, &index),
        Tracking::TrackedUnverified
    );
}

#[test]
fn chezmoi_tmpl_is_tracked_no_drift() {
    let f = Fixture::new();
    let repo = f.dir("cz");
    write(&repo.join("dot_zshrc.tmpl"), "{{ .x }}");
    f.dir("home");
    let h = f.root().join("home/.zshrc");
    write(&h, "rendered differently");
    let index = Index::build(&repo, Layout::Chezmoi).unwrap();
    let e = index.lookup(Path::new(".zshrc")).unwrap();
    assert!(e.template && !e.unverified);
    assert_eq!(
        decide(&h, Path::new(".zshrc"), &repo, &index),
        Tracking::Tracked
    );
}

fn build_with_timeout(repo: &Path) -> Index {
    let repo = repo.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(Index::build(&repo, Layout::Chezmoi));
    });
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .expect("Index::build hung")
        .unwrap()
}

fn paths(index: &Index) -> Vec<String> {
    index
        .entries()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn chezmoiroot_is_honored() {
    let f = Fixture::new();
    let repo = f.dir("cz");
    write(&repo.join(".chezmoiroot"), "home\n");
    write(&repo.join("home/dot_zshrc"), "a");
    write(&repo.join("dot_ignored"), "a");
    assert_eq!(paths(&build_with_timeout(&repo)), vec![".zshrc"]);
}

#[test]
fn chezmoiroot_invalid_values_are_ignored_and_never_escape() {
    let f = Fixture::new();
    let outside = f.dir("outside");
    write(&outside.join("dot_evil"), "a");
    write(&f.root().join("dot_evil2"), "a");
    for value in [
        outside.to_string_lossy().into_owned(),
        "../outside".to_string(),
        "".to_string(),
        "missing".to_string(),
    ] {
        let repo = f.dir("cz");
        write(&repo.join("dot_in"), "a");
        write(&repo.join(".chezmoiroot"), &value);
        assert_eq!(
            paths(&build_with_timeout(&repo)),
            vec![".in"],
            "value {value:?}"
        );
        fs::remove_dir_all(&repo).unwrap();
    }
}

#[test]
fn chezmoiroot_symlinked_file_or_subdir_is_ignored() {
    let f = Fixture::new();
    let outside = f.dir("outside");
    write(&outside.join("dot_evil"), "a");
    write(&outside.join("rootfile"), "sub");
    let repo = f.dir("cz");
    write(&repo.join("dot_in"), "a");
    write(&repo.join("sub/dot_inner"), "a");
    // symlinked .chezmoiroot pointing at a file naming a real subdir
    std::os::unix::fs::symlink(outside.join("rootfile"), repo.join(".chezmoiroot")).unwrap();
    assert_eq!(paths(&build_with_timeout(&repo)), vec![".in", "sub/.inner"]);
    fs::remove_file(repo.join(".chezmoiroot")).unwrap();
    // regular .chezmoiroot naming a symlink to an outside dir
    std::os::unix::fs::symlink(&outside, repo.join("link")).unwrap();
    write(&repo.join(".chezmoiroot"), "link");
    assert_eq!(paths(&build_with_timeout(&repo)), vec![".in", "sub/.inner"]);
}

#[test]
fn chezmoiroot_intermediate_symlink_is_ignored() {
    let f = Fixture::new();
    let outside = f.dir("outside");
    write(&outside.join("b/dot_evil"), "a");
    let repo = f.dir("cz");
    write(&repo.join("dot_in"), "a");
    std::os::unix::fs::symlink(&outside, repo.join("a")).unwrap();
    write(&repo.join(".chezmoiroot"), "a/b");
    assert_eq!(paths(&build_with_timeout(&repo)), vec![".in"]);
}

#[test]
fn chezmoiroot_nested_real_dirs_are_honored() {
    let f = Fixture::new();
    let repo = f.dir("cz");
    write(&repo.join("dot_top"), "a");
    write(&repo.join("src/home/dot_zshrc"), "a");
    write(&repo.join(".chezmoiroot"), "src/home");
    assert_eq!(paths(&build_with_timeout(&repo)), vec![".zshrc"]);
}

#[test]
fn chezmoiroot_fifo_does_not_hang() {
    let f = Fixture::new();
    let repo = f.dir("cz");
    write(&repo.join("dot_in"), "a");
    f.fifo("cz/.chezmoiroot");
    assert_eq!(paths(&build_with_timeout(&repo)), vec![".in"]);
}

#[test]
fn decide_does_not_hang_on_fifo_home_path() {
    let f = Fixture::new();
    let repo = f.dir("dotfiles");
    write(&repo.join("zsh/.zshrc"), "a");
    let fifo = f.fifo("home/.zshrc");
    let index = Index::build(&repo, Layout::Stow).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let repo2 = repo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(decide(&fifo, Path::new(".zshrc"), &repo2, &index));
    });
    let got = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("decide hung");
    assert_eq!(got, Tracking::Untracked);
}

#[test]
fn unit_aggregation_all_some_none() {
    use Tracking::*;
    assert_eq!(unit_tracking(&[]), Untracked);
    assert_eq!(unit_tracking(&[Tracked, Tracked]), Tracked);
    assert_eq!(unit_tracking(&[Tracked, Untracked]), PartiallyTracked);
    assert_eq!(unit_tracking(&[Untracked, Untracked]), Untracked);
    assert_eq!(unit_tracking(&[Tracked, TrackedDrifted]), TrackedDrifted);
    assert_eq!(
        unit_tracking(&[TrackedUnverified, Tracked]),
        TrackedUnverified
    );
    assert_eq!(
        unit_tracking(&[TrackedUnverified, TrackedDrifted]),
        TrackedDrifted
    );
    assert_eq!(
        unit_tracking(&[TrackedDrifted, Untracked]),
        PartiallyTracked
    );
}

#[test]
fn repo_file_missing_on_machine_is_listed() {
    let f = Fixture::new();
    let repo = f.dir("dotfiles");
    write(&repo.join("zsh/.zshrc"), "a");
    write(&repo.join("zsh/.zprofile"), "a");
    write(&repo.join("vim/.vimrc"), "a");
    write(&repo.join("x/.dangling"), "a");
    f.dir("home");
    write(&f.root().join("home/.zshrc"), "a");
    f.symlink("home/.dangling", Path::new("/nonexistent/target"));
    let index = Index::build(&repo, Layout::Stow).unwrap();
    let missing = missing_on_machine(&index, &f.root().join("home"));
    assert_eq!(
        missing,
        vec![PathBuf::from(".vimrc"), PathBuf::from(".zprofile")]
    );
}

#[test]
fn index_build_missing_dir_is_error() {
    let f = Fixture::new();
    assert!(Index::build(&f.root().join("nope"), Layout::Stow).is_err());
    assert!(Index::build(&f.root().join("nope"), Layout::Chezmoi).is_err());
    let file = f.file("afile");
    assert!(Index::build(&file, Layout::Stow).is_err());
}

#[test]
fn stow_index_skips_dot_dirs_and_does_not_follow_symlinks() {
    let f = Fixture::new();
    let repo = f.dir("dotfiles");
    write(&repo.join(".git/x/.hidden"), "a");
    write(&repo.join("README"), "a");
    write(&repo.join("zsh/.git/y"), "a");
    write(&repo.join("zsh/.zshrc"), "a");
    let outside = f.dir("outside");
    write(&outside.join("secret"), "a");
    std::os::unix::fs::symlink(&outside, repo.join("zsh/linkdir")).unwrap();
    let index = Index::build(&repo, Layout::Stow).unwrap();
    assert_eq!(index.entries().count(), 1);
    assert!(index.lookup(Path::new(".zshrc")).is_some());
}

struct RestoreDir(PathBuf);
impl Drop for RestoreDir {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

fn running_as_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[test]
fn unreadable_subdirs_are_recorded_and_build_continues() {
    if running_as_root() {
        eprintln!("SKIPPED: running as root, chmod 000 does not block reads");
        return;
    }
    let f = Fixture::new();
    // stow: an unreadable package dir and an unreadable nested dir
    let repo = f.dir("stow");
    write(&repo.join("zsh/.zshrc"), "a");
    write(&repo.join("lockedpkg/.x"), "a");
    write(&repo.join("git/.config/git/locked/y"), "a");
    write(&repo.join("git/.gitconfig"), "a");
    let pkg = repo.join("lockedpkg");
    let nested = repo.join("git/.config/git/locked");
    let _g1 = RestoreDir(pkg.clone());
    let _g2 = RestoreDir(nested.clone());
    f.chmod(&pkg, 0o000);
    f.chmod(&nested, 0o000);
    let index = Index::build(&repo, Layout::Stow).expect("subdir denial is not fatal");
    assert_eq!(index.unreadable(), &[nested.clone(), pkg.clone()][..]);
    assert!(index.lookup(Path::new(".zshrc")).is_some());
    assert!(index.lookup(Path::new(".gitconfig")).is_some());

    // chezmoi: an unreadable source subdir
    let chez = f.dir("chez");
    write(&chez.join("dot_zshrc"), "a");
    write(&chez.join("dot_config/locked/x"), "a");
    write(&chez.join("dot_config/ok/y"), "a");
    let locked = chez.join("dot_config/locked");
    let _g3 = RestoreDir(locked.clone());
    f.chmod(&locked, 0o000);
    let index = Index::build(&chez, Layout::Chezmoi).expect("subdir denial is not fatal");
    assert_eq!(index.unreadable(), &[locked.clone()][..]);
    assert!(index.lookup(Path::new(".zshrc")).is_some());
    assert!(index.lookup(Path::new(".config/ok/y")).is_some());
}

#[test]
fn unreadable_top_level_dotfiles_dir_is_still_an_error() {
    if running_as_root() {
        eprintln!("SKIPPED: running as root, chmod 000 does not block reads");
        return;
    }
    let f = Fixture::new();
    let repo = f.dir("stow");
    write(&repo.join("zsh/.zshrc"), "a");
    let _g = RestoreDir(repo.clone());
    f.chmod(&repo, 0o000);
    assert!(Index::build(&repo, Layout::Stow).is_err());
    assert!(Index::build(&repo, Layout::Chezmoi).is_err());
}

#[test]
fn decide_resolves_a_non_canonical_dotfiles_path() {
    let f = Fixture::new();
    let repo = f.dir("dotfiles");
    let target = repo.join("zsh/.zshrc");
    write(&target, "a");
    f.dir("home");
    let link = f.symlink("home/.zshrc", &target);
    let index = Index::build(&repo, Layout::Stow).unwrap();
    assert_eq!(
        decide(&link, Path::new(".zshrc"), &repo.join("zsh/.."), &index),
        Tracking::Tracked
    );
}

#[test]
fn stow_control_files_at_a_package_top_are_not_indexed() {
    let f = Fixture::new();
    let repo = f.dir("dotfiles");
    write(&repo.join("karabiner/.stow-local-ignore"), "README.*\n");
    write(&repo.join("karabiner/.stow-global-ignore"), "x\n");
    write(
        &repo.join("karabiner/.config/karabiner/karabiner.json"),
        "{}",
    );
    // Deeper inside a package it is an ordinary file that stow links.
    write(&repo.join("karabiner/.config/x/.stow-local-ignore"), "y\n");
    write(&repo.join("zsh/.zshrc"), "a");
    let index = Index::build(&repo, Layout::Stow).unwrap();
    assert!(index.lookup(Path::new(".stow-local-ignore")).is_none());
    assert!(index.lookup(Path::new(".stow-global-ignore")).is_none());
    assert!(index
        .lookup(Path::new(".config/x/.stow-local-ignore"))
        .is_some());
    assert_eq!(index.entries().count(), 3);
    let home = f.dir("home");
    let missing = missing_on_machine(&index, &home);
    assert_eq!(
        missing,
        vec![
            PathBuf::from(".config/karabiner/karabiner.json"),
            PathBuf::from(".config/x/.stow-local-ignore"),
            PathBuf::from(".zshrc"),
        ]
    );
    // The chezmoi layout is unaffected: a source mapping to that name is
    // an ordinary target.
    let cz = f.dir("cz");
    write(&cz.join("dot_stow-local-ignore"), "z\n");
    let index = Index::build(&cz, Layout::Chezmoi).unwrap();
    assert!(index.lookup(Path::new(".stow-local-ignore")).is_some());
}
