#![allow(dead_code)]

use std::path::Path;

use crate::model::Kind;
use crate::rules::Rules;

const CACHE_DIRS: &[&str] = &[
    "Caches",
    "cache",
    ".cache",
    "node_modules",
    "logs",
    "tmp",
    "GPUCache",
    "Crashpad",
];

pub fn classify(home_rel: &Path, rules: &Rules) -> (Option<String>, Kind) {
    if let Some(m) = rules.lookup(home_rel) {
        return (Some(m.app), m.kind);
    }
    let in_cache_dir = home_rel
        .parent()
        .map(|p| {
            p.components()
                .any(|c| CACHE_DIRS.iter().any(|d| c.as_os_str() == *d))
        })
        .unwrap_or(false);
    if in_cache_dir {
        return (None, Kind::Cache);
    }
    let is_db = home_rel
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e, "sqlite" | "sqlite3" | "db"))
        .unwrap_or(false);
    if is_db {
        return (None, Kind::State);
    }
    (None, Kind::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Rules;

    fn k(p: &str) -> (Option<String>, Kind) {
        classify(Path::new(p), &Rules::embedded())
    }

    #[test]
    fn embedded_rules_parse() {
        assert!(Rules::parse(include_str!("../rules/apps.toml")).is_ok());
        assert!(Rules::embedded().lookup(Path::new(".zshrc")).is_some());
    }

    #[test]
    fn ngrok_yml_is_config_with_app_name() {
        assert_eq!(k(".config/ngrok/ngrok.yml"), (Some("ngrok".into()), Kind::Config));
    }

    #[test]
    fn rule_overrides_heuristic() {
        let rules = Rules::parse(
            "[[rule]]\napp=\"x\"\nkind=\"config\"\nglobs=[\".cache/x/**\"]\ndescription=\"d\"\n",
        )
        .unwrap();
        assert_eq!(
            classify(Path::new(".cache/x/a"), &rules),
            (Some("x".into()), Kind::Config)
        );
    }

    #[test]
    fn cache_dirs_are_cache() {
        for p in [
            "Library/Application Support/Foo/GPUCache/x",
            ".cache/y",
            "node_modules/z",
        ] {
            assert_eq!(k(p), (None, Kind::Cache), "{p}");
        }
    }

    #[test]
    fn sqlite_is_state() {
        for p in [".foo/data.sqlite", ".foo/data.db", ".foo/data.sqlite3"] {
            assert_eq!(k(p), (None, Kind::State), "{p}");
        }
    }

    #[test]
    fn unmatched_is_unknown() {
        assert_eq!(k(".config/mystery/a.conf"), (None, Kind::Unknown));
    }
}
