mod common;

use std::fs;
use std::time::Instant;

use common::Fixture;
use dredge::secrets::{scan_file, SecretRules, MAX_SCAN_BYTES};

const FAKE: &str = "FAKE_TOKEN_9f8e7d6c5b4a3210";

#[test]
fn embedded_rules_parse() {
    let rules = SecretRules::parse(include_str!("../rules/secrets.toml")).unwrap();
    assert_eq!(rules.len(), 4);
    assert_eq!(SecretRules::embedded().len(), 4);
}

#[test]
fn finds_ngrok_token_with_line_number() {
    let fx = Fixture::new();
    let p = fx.root().join("ngrok.yml");
    fs::write(&p, format!("version: 2\r\nauthtoken: {FAKE}\r\n")).unwrap();
    let found = scan_file(&p, &SecretRules::embedded()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].rule_id, "ngrok-authtoken");
    assert_eq!(found[0].line, 2);
    assert_eq!(found[0].path, p);
}

#[test]
fn finding_debug_and_serialized_output_never_contains_value() {
    let fx = Fixture::new();
    let p = fx.root().join("ngrok.yml");
    fs::write(&p, format!("authtoken: {FAKE}\n")).unwrap();
    let found = scan_file(&p, &SecretRules::embedded()).unwrap();
    assert_eq!(found.len(), 1);
    let debug = format!("{found:?}");
    let json = serde_json::to_string(&found).unwrap();
    assert!(!debug.contains(FAKE));
    assert!(!json.contains(FAKE));
    assert!(!debug.contains("9f8e7d6c"));
    assert!(!json.contains("9f8e7d6c"));
}

#[test]
fn skips_binary_files() {
    let fx = Fixture::new();
    let p = fx.root().join("bin");
    let mut data = format!("authtoken: {FAKE}\n").into_bytes();
    data.push(0);
    fs::write(&p, data).unwrap();
    assert!(scan_file(&p, &SecretRules::embedded()).unwrap().is_empty());
}

#[test]
fn skips_files_over_1mib() {
    let fx = Fixture::new();
    let p = fx.root().join("big");
    let mut data = format!("authtoken: {FAKE}\n").into_bytes();
    data.resize(MAX_SCAN_BYTES as usize + 1, b'a');
    fs::write(&p, data).unwrap();
    assert!(scan_file(&p, &SecretRules::embedded()).unwrap().is_empty());
}

#[test]
fn skips_symlinks_and_fifos() {
    let fx = Fixture::new();
    let target = fx.root().join("real");
    fs::write(&target, format!("authtoken: {FAKE}\n")).unwrap();
    let link = fx.symlink("link", &target);
    let fifo = fx.fifo("pipe");
    let rules = SecretRules::embedded();
    assert!(scan_file(&link, &rules).unwrap().is_empty());
    assert!(scan_file(&fifo, &rules).unwrap().is_empty());
}

#[test]
fn disabled_generic_rules_do_not_fire() {
    let fx = Fixture::new();
    let p = fx.root().join("conf");
    fs::write(&p, "password = hunter2\ntoken: abc\n").unwrap();
    assert!(scan_file(&p, &SecretRules::embedded()).unwrap().is_empty());
}

#[test]
fn single_900kb_line_scans_in_under_one_second() {
    let fx = Fixture::new();
    let p = fx.root().join("long");
    fs::write(&p, vec![b'a'; 900_000]).unwrap();
    let rules = SecretRules::embedded();
    let start = Instant::now();
    let found = scan_file(&p, &rules).unwrap();
    let t = start.elapsed().as_secs_f64();
    eprintln!("ascii 900kb: {t:.3}s");
    assert!(t < 1.0);
    assert!(found.is_empty());
}

#[test]
fn non_ascii_900kb_line_scans_in_under_one_second() {
    let fx = Fixture::new();
    let p = fx.root().join("uni");
    fs::write(&p, "é".repeat(450_000)).unwrap();
    let rules = SecretRules::embedded();
    let start = Instant::now();
    let found = scan_file(&p, &rules).unwrap();
    let t = start.elapsed().as_secs_f64();
    eprintln!("non-ascii 900kb: {t:.3}s");
    assert!(t < 1.0);
    assert!(found.is_empty());
}

#[test]
fn many_matches_on_900kb_line_scan_in_under_one_second() {
    let fx = Fixture::new();
    let p = fx.root().join("many");
    // 21 bytes per key plus separator: 40_000 keys is about 880 KB.
    fs::write(&p, "AKIAABCDEFGHIJKLMNOP ".repeat(40_000)).unwrap();
    let rules = SecretRules::embedded();
    let start = Instant::now();
    let found = scan_file(&p, &rules).unwrap();
    let t = start.elapsed().as_secs_f64();
    eprintln!("many matches: {t:.3}s");
    assert!(t < 1.0);
    assert_eq!(found.len(), 40_000);
    assert!(found.iter().all(|f| f.line == 1));
}

#[test]
fn open_regular_refuses_symlink_and_fifo_without_blocking() {
    let fx = Fixture::new();
    let target = fx.root().join("real");
    fs::write(&target, "x").unwrap();
    let link = fx.symlink("link", &target);
    let fifo = fx.fifo("pipe");
    assert!(dredge::secrets::open_regular(&link).unwrap().is_none());
    assert!(dredge::secrets::open_regular(&fifo).unwrap().is_none());
    assert!(dredge::secrets::open_regular(&target).unwrap().is_some());
}

#[test]
fn multiple_matches_one_line_and_repeats_report_each_with_correct_lines() {
    let fx = Fixture::new();
    let p = fx.root().join("multi");
    let aws = "AKIAABCDEFGHIJKLMNOP";
    let body =
        format!("{aws} and {aws} again\nnothing\r\n-----BEGIN RSA PRIVATE KEY-----\n{aws}\n");
    fs::write(&p, body).unwrap();
    let found = scan_file(&p, &SecretRules::embedded()).unwrap();
    let got: Vec<(&str, usize)> = found.iter().map(|f| (f.rule_id.as_str(), f.line)).collect();
    assert_eq!(
        got,
        vec![
            ("aws-access-key-id", 1),
            ("aws-access-key-id", 1),
            ("private-key", 3),
            ("aws-access-key-id", 4),
        ]
    );
}
