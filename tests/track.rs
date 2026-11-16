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

#[test]
fn chezmoiroot_is_honored_and_does_not_crash() {
    let f = Fixture::new();
    let repo = f.dir("cz");
    write(&repo.join(".chezmoiroot"), "home\n");
    write(&repo.join("home/dot_zshrc"), "a");
    write(&repo.join("dot_ignored"), "a");
    let index = Index::build(&repo, Layout::Chezmoi).unwrap();
    assert!(index.lookup(Path::new(".zshrc")).is_some());
    assert!(index.lookup(Path::new(".ignored")).is_none());
    write(&repo.join(".chezmoiroot"), "../nope");
    assert!(Index::build(&repo, Layout::Chezmoi).is_ok());
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
