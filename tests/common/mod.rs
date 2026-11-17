#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

pub struct Fixture {
    pub tmp: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Fixture {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    pub fn root(&self) -> PathBuf {
        fs::canonicalize(self.tmp.path()).unwrap()
    }

    pub fn file(&self, rel: &str) -> PathBuf {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"x").unwrap();
        p
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.root().join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }

    pub fn symlink(&self, rel: &str, target: &Path) -> PathBuf {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        symlink(target, &p).unwrap();
        p
    }

    pub fn fifo(&self, rel: &str) -> PathBuf {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let status = Command::new("mkfifo").arg(&p).status().unwrap();
        assert!(status.success());
        p
    }

    pub fn chmod(&self, path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
}
