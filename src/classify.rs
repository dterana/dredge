use std::ffi::OsStr;
use std::path::Path;

use crate::model::Kind;
use crate::rules::Rules;

/// Directory names whose contents are cache or app state. Matched ASCII case
/// insensitively, since macOS file systems usually are.
const CACHE_DIRS: &[&str] = &[
    "Caches",
    "cache",
    ".cache",
    "node_modules",
    "logs",
    "tmp",
    "GPUCache",
    "Crashpad",
    // Chromium and Electron profile storage
    "Code Cache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    "GrShaderCache",
    "ShaderCache",
    "Service Worker",
    "IndexedDB",
    "Local Storage",
    "Session Storage",
    "blob_storage",
    "File System",
    "WebStorage",
    "Shared Dictionary",
    "component_crx_cache",
    "Crash Reports",
    // Visual Studio Code and its forks
    "CachedData",
    "CachedExtensions",
    "CachedExtensionVSIXs",
];

/// True if a directory with this name holds cache or app state. The single
/// test used both by classification and by the walker's pruning, so the two
/// never disagree. `Cache` is covered by the case-insensitive `cache`.
pub fn is_cache_dir_name(name: &OsStr) -> bool {
    CACHE_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

/// True if the walker must not enter the directory `home_rel_dir`. A matching
/// rule decides, as in `classify` (a child path is probed so `dir/**` globs
/// match the directory itself): cache or state prunes, anything else does not.
/// Without a rule, the directory's own name decides.
pub fn prunes_dir(home_rel_dir: &Path, rules: &Rules) -> bool {
    match rules.lookup(&home_rel_dir.join("x")) {
        Some(m) => matches!(m.kind, Kind::Cache | Kind::State),
        None => home_rel_dir.file_name().is_some_and(is_cache_dir_name),
    }
}

pub fn classify(home_rel: &Path, rules: &Rules) -> (Option<String>, Kind) {
    if let Some(m) = rules.lookup(home_rel) {
        return (Some(m.app), m.kind);
    }
    let in_cache_dir = home_rel
        .parent()
        .map(|p| p.components().any(|c| is_cache_dir_name(c.as_os_str())))
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
        assert_eq!(
            k(".config/ngrok/ngrok.yml"),
            (Some("ngrok".into()), Kind::Config)
        );
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
    fn chromium_and_editor_storage_dirs_are_cache() {
        for name in [
            "Cache",
            "Code Cache",
            "DawnCache",
            "DawnGraphiteCache",
            "DawnWebGPUCache",
            "GrShaderCache",
            "ShaderCache",
            "Service Worker",
            "IndexedDB",
            "Local Storage",
            "Session Storage",
            "blob_storage",
            "File System",
            "WebStorage",
            "Shared Dictionary",
            "component_crx_cache",
            "Crash Reports",
            "CachedData",
            "CachedExtensions",
            "CachedExtensionVSIXs",
        ] {
            let p = format!("Library/Application Support/Notion/{name}/sub/f.txt");
            assert_eq!(k(&p), (None, Kind::Cache), "{p}");
            assert!(is_cache_dir_name(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn cache_dir_names_match_ascii_case_insensitively() {
        for name in [
            "cache",
            "CACHE",
            "Cache",
            "caches",
            "LOGS",
            "Tmp",
            "indexeddb",
            "LOCAL STORAGE",
            "Node_Modules",
        ] {
            assert!(is_cache_dir_name(OsStr::new(name)), "{name}");
            assert_eq!(k(&format!(".foo/{name}/x")), (None, Kind::Cache), "{name}");
        }
    }

    #[test]
    fn near_miss_names_are_not_cache() {
        for name in [
            "Cachet",
            "my cache",
            "Storage",
            "Local",
            "Partitions",
            "log",
        ] {
            assert!(!is_cache_dir_name(OsStr::new(name)), "{name}");
        }
        // only directories count: a file named like a cache dir is not cache
        assert_eq!(k(".foo/Cache"), (None, Kind::Unknown));
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
