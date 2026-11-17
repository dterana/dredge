use std::collections::HashSet;
use std::fs;
use std::path::Path;

use dredge::classify::classify;
use dredge::model::Kind;
use dredge::rules::Rules;
use dredge::secrets::{scan_file, SecretRules};

const APPS: &str = include_str!("../rules/apps.toml");
const SECRETS: &str = include_str!("../rules/secrets.toml");

#[test]
fn every_rule_has_app_kind_globs_and_description() {
    let defs = Rules::parse_defs(APPS).unwrap();
    assert!(defs.len() >= 50, "expected a seeded rule set");
    for d in &defs {
        assert!(!d.app.trim().is_empty(), "empty app");
        assert!(
            !d.description.trim().is_empty(),
            "{}: no description",
            d.app
        );
        assert!(!d.globs.is_empty(), "{}: no globs", d.app);
        assert!(
            d.globs.iter().all(|g| !g.trim().is_empty()),
            "{}: blank glob",
            d.app
        );
        assert!(!d.description.contains('\u{2014}'), "{}: em dash", d.app);
    }
    assert!(Rules::parse(APPS).is_ok(), "globs must compile");
    let apps: HashSet<&str> = defs.iter().map(|d| d.app.as_str()).collect();
    assert!(
        apps.len() >= 50,
        "expected about 50 apps, got {}",
        apps.len()
    );
}

#[test]
fn no_duplicate_app_globs() {
    let defs = Rules::parse_defs(APPS).unwrap();
    let mut seen = HashSet::new();
    for d in &defs {
        for g in &d.globs {
            assert!(seen.insert(g.clone()), "duplicate glob {g} (app {})", d.app);
        }
    }
}

/// Builds a concrete path that a glob matches, to detect rules shadowed by an earlier rule.
fn sample(glob: &str) -> String {
    glob.replace("/**", "/x").replace('*', "x")
}

#[test]
fn no_glob_is_shadowed_by_an_earlier_rule() {
    let defs = Rules::parse_defs(APPS).unwrap();
    let rules = Rules::parse(APPS).unwrap();
    for d in &defs {
        for g in &d.globs {
            let m = rules.lookup(Path::new(&sample(g))).unwrap();
            assert_eq!(
                (m.app.as_str(), m.kind, m.description.as_str()),
                (d.app.as_str(), d.kind, d.description.as_str()),
                "glob {g} of app {} is shadowed",
                d.app
            );
        }
    }
}

#[test]
fn every_secret_rule_has_id_regex_and_source() {
    let defs = SecretRules::parse_defs(SECRETS).unwrap();
    assert!(defs.len() >= 15);
    for d in &defs {
        assert!(!d.id.trim().is_empty());
        assert!(!d.regex.trim().is_empty(), "{}: empty regex", d.id);
        assert!(
            d.source == "gitleaks" || d.source == "dredge",
            "{}: unexpected source {}",
            d.id,
            d.source
        );
    }
}

#[test]
fn every_secret_rule_regex_compiles() {
    for d in SecretRules::parse_defs(SECRETS).unwrap() {
        regex::bytes::Regex::new(&d.regex)
            .unwrap_or_else(|e| panic!("rule {} does not compile: {e}", d.id));
    }
}

#[test]
fn rule_ids_are_unique() {
    let defs = SecretRules::parse_defs(SECRETS).unwrap();
    let mut seen = HashSet::new();
    for d in &defs {
        assert!(seen.insert(d.id.clone()), "duplicate id {}", d.id);
    }
}

#[test]
fn no_enabled_entropy_rule() {
    let defs = SecretRules::parse_defs(SECRETS).unwrap();
    let generic = defs
        .iter()
        .find(|d| d.id == "generic-secret-assignment")
        .unwrap();
    assert!(!generic.enabled);
    for d in &defs {
        if d.id.contains("generic") || d.id.contains("entropy") {
            assert!(!d.enabled, "{} must stay disabled", d.id);
        }
    }
}

fn rep(c: &str, n: usize) -> String {
    c.repeat(n)
}

/// (rule id, line that must fire only that rule, near-miss line that must fire nothing).
/// Values are built from fragments so this file holds no complete token-shaped string.
fn cases() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "private-key",
            format!("{} {} KEY{}", "-----BEGIN", "RSA PRIVATE", "-----"),
            format!("{} {} KEY{}", "-----BEGIN", "PUBLIC", "-----"),
        ),
        (
            "aws-access-key-id",
            format!("key = {}{}", "AKIA", rep("A", 16)),
            format!("key = {}{}", "AKIA", rep("A", 10)),
        ),
        (
            "aws-secret-access-key",
            format!("{} = {}", "aws_secret_access_key", rep("a", 40)),
            format!("{} = {}", "aws_secret_access_key", rep("a", 20)),
        ),
        (
            "github-token",
            format!("t={}{}", "ghp_", rep("a", 36)),
            format!("t={}{}", "ghp_", rep("a", 10)),
        ),
        (
            "github-fine-grained-pat",
            format!("t={}{}", "github_pat_", rep("A", 82)),
            format!("t={}{}", "github_pat_", rep("A", 20)),
        ),
        (
            "gitlab-pat",
            format!("t={}{}", "glpat-", rep("a", 20)),
            format!("t={}{}", "glpat-", rep("a", 8)),
        ),
        (
            "slack-bot-token",
            format!("t={}{}-{}", "xoxb-", rep("1", 11), rep("2", 11)),
            format!("t={}{}", "xoxb-", rep("1", 4)),
        ),
        (
            "slack-user-token",
            format!(
                "t={}-{}-{}-{}-{}",
                "xoxp",
                rep("1", 11),
                rep("2", 11),
                rep("3", 11),
                rep("a", 30)
            ),
            format!("t={}-{}", "xoxp", rep("1", 11)),
        ),
        (
            "slack-app-token",
            format!(
                "t={}1-{}-{}-{}",
                "xapp-",
                "A0B1C2D3E4",
                rep("9", 8),
                rep("a", 16)
            ),
            format!("t={}{}", "xapp-", "1-"),
        ),
        (
            "slack-webhook-url",
            format!(
                "u=https://{}/services/T{}/B{}/{}",
                "hooks.slack.com",
                rep("A", 9),
                rep("B", 9),
                rep("c", 24)
            ),
            format!("u=https://{}/services/", "hooks.slack.com"),
        ),
        (
            "stripe-live-key",
            format!("k={}{}", "sk_live_", rep("a", 24)),
            format!("k={}{}", "sk_test_", rep("a", 24)),
        ),
        (
            "google-api-key",
            format!("k={}{}", "AIza", rep("a", 35)),
            format!("k={}{}", "AIza", rep("a", 12)),
        ),
        (
            "npm-access-token",
            format!("//registry/:_authToken={}{}", "npm_", rep("a", 36)),
            format!("//registry/:_authToken={}{}", "npm_", rep("a", 12)),
        ),
        (
            "pypi-upload-token",
            format!("password = {}{}", "pypi-AgEIcHlwaS5vcmc", rep("A", 60)),
            format!("password = {}", "pypi-AgEIcHlwaS5vcmc"),
        ),
        (
            "sendgrid-api-key",
            format!("k={}{}.{}", "SG.", rep("a", 22), rep("b", 43)),
            format!("k={}{}", "SG.", rep("a", 22)),
        ),
        (
            "twilio-api-key",
            format!("k={}{}", "SK", rep("a", 32)),
            format!("k={}{}", "SK", rep("a", 20)),
        ),
        (
            "shopify-access-token",
            format!("k={}{}", "shpat_", rep("a", 32)),
            format!("k={}{}", "shpat_", rep("a", 12)),
        ),
        (
            "openai-api-key",
            format!("k={}{}{}{}", "sk-", rep("a", 20), "T3BlbkFJ", rep("b", 20)),
            format!("k={}{}", "sk-", rep("a", 20)),
        ),
        (
            "anthropic-api-key",
            format!("k={}{}AA", "sk-ant-api03-", rep("a", 93)),
            format!("k={}{}", "sk-ant-api03-", rep("a", 20)),
        ),
        (
            "digitalocean-token",
            format!("k={}{}", "dop_v1_", rep("a", 64)),
            format!("k={}{}", "dop_v1_", rep("a", 20)),
        ),
        (
            "age-secret-key",
            format!("{}{}", "AGE-SECRET-KEY-1", rep("Q", 58)),
            format!("{}{}", "AGE-SECRET-KEY-1", rep("Q", 20)),
        ),
        (
            "ngrok-authtoken",
            format!("authtoken: {}", rep("a1", 15)),
            "authtoken: short".to_string(),
        ),
    ]
}

#[test]
fn every_enabled_rule_has_a_detection_case_and_only_it_fires() {
    let defs = SecretRules::parse_defs(SECRETS).unwrap();
    let enabled: HashSet<&str> = defs
        .iter()
        .filter(|d| d.enabled)
        .map(|d| d.id.as_str())
        .collect();
    let cases = cases();
    let covered: HashSet<&str> = cases.iter().map(|c| c.0).collect();
    assert_eq!(
        enabled, covered,
        "every enabled rule needs exactly one case"
    );

    let rules = SecretRules::embedded();
    assert_eq!(rules.len(), enabled.len());
    let dir = tempfile::tempdir().unwrap();
    for (id, hit, miss) in cases {
        let p = dir.path().join("hit.txt");
        fs::write(&p, format!("{hit}\n")).unwrap();
        let found = scan_file(&p, &rules).unwrap();
        let ids: Vec<&str> = found.iter().map(|f| f.rule_id.as_str()).collect();
        assert_eq!(ids, vec![id], "hit line for {id}");

        fs::write(&p, format!("{miss}\n")).unwrap();
        let found = scan_file(&p, &rules).unwrap();
        assert!(found.is_empty(), "near miss for {id} fired {found:?}");
    }
}

fn class(p: &str) -> (Option<String>, Kind) {
    classify(Path::new(p), &Rules::embedded())
}

#[test]
fn gcloud_config_rule_covers_only_configurations() {
    let config = (Some("gcloud".to_string()), Kind::Config);
    assert_eq!(
        class(".config/gcloud/configurations/config_default"),
        config
    );
    assert_eq!(class(".config/gcloud/active_config"), config);
    assert_eq!(
        class(".config/gcloud/logs/2026.11.16/x.log"),
        (Some("gcloud".to_string()), Kind::State)
    );
    // Credential stores are never nudged toward tracking as config.
    for p in [
        ".config/gcloud/access_tokens.db",
        ".config/gcloud/credentials.db",
        ".config/gcloud/legacy_credentials/me@example.com/adc.json",
        ".config/gcloud/application_default_credentials.json",
    ] {
        let (app, kind) = class(p);
        assert_ne!(kind, Kind::Config, "{p}");
        assert_eq!(app, None, "{p}");
    }
}

#[test]
fn macos_system_files_are_state() {
    let state = (Some("macos".to_string()), Kind::State);
    for p in [
        ".DS_Store",
        ".CFUserTextEncoding",
        ".localized",
        ".Trash/old.txt",
        ".Trash/dir/nested.bin",
    ] {
        assert_eq!(class(p), state, "{p}");
    }
}

fn fired(line: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("sample.txt");
    fs::write(&p, format!("{line}\n")).unwrap();
    scan_file(&p, &SecretRules::embedded())
        .unwrap()
        .into_iter()
        .map(|f| f.rule_id)
        .collect()
}

#[test]
fn ngrok_authtoken_matches_yaml_and_env_forms_but_not_npm() {
    let fake = rep("a1", 15);
    assert_eq!(
        fired(&format!("authtoken: {fake}")),
        vec!["ngrok-authtoken"]
    );
    assert_eq!(
        fired(&format!("export {}={fake}", "NGROK_AUTHTOKEN")),
        vec!["ngrok-authtoken"]
    );
    assert_eq!(
        fired(&format!("{}=\"{fake}\"", "ngrok_authtoken")),
        vec!["ngrok-authtoken"]
    );
    // npm's `_authToken` is not an ngrok token.
    let npm = format!("{}{}", "npm_", rep("a", 36));
    assert_eq!(
        fired(&format!("//registry.npmjs.org/:_authToken={npm}")),
        vec!["npm-access-token"]
    );
    assert!(fired(&format!("//registry.npmjs.org/:_authToken={fake}")).is_empty());
    assert!(fired(&format!("MY_AUTHTOKEN={fake}")).is_empty());
}
