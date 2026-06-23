use codez_llm_compress::ccr::{attach, CcrRegistry, RequestCtx};
use codez_llm_compress::config::CcrCfg;
use serial_test::serial;
use std::cell::RefCell;
use std::collections::HashMap;

fn ctx<'a>(queryid: &'a str) -> RequestCtx<'a> {
    RequestCtx {
        queryid,
        cmd_index: HashMap::new(),
        ccr: RefCell::new(CcrRegistry::new()),
    }
}

fn cfg_enabled() -> CcrCfg {
    CcrCfg { enabled: true, max_files_per_thread: 200, max_thread_bytes: 67_108_864, max_file_bytes: 4_194_304 }
}

#[test]
#[serial]
fn enabled_writes_file_and_appends_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-abc");
    let original = "VERY LONG ORIGINAL CONTENT ".repeat(50);
    let compressed = "[llm-compress: 略 49 行]".to_string();
    let out = attach(compressed.clone(), &original, &c, "call1", &cfg_enabled());
    // Appended original text path to placeholder
    assert!(out.contains("[llm-compress: 原文 "), "contains path placeholder");
    assert!(out.starts_with("[llm-compress: 略 49 行]"));
    // File content at path == original text
    let path_part = out.split("原文 ").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let written = std::fs::read_to_string(path_part).unwrap();
    assert_eq!(written, original);
}

#[test]
#[serial]
fn disabled_returns_compressed_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-x");
    let mut cfg = cfg_enabled();
    cfg.enabled = false;
    let compressed = "[llm-compress: 略 N 行]".to_string();
    let out = attach(compressed.clone(), "original", &c, "call1", &cfg);
    assert_eq!(out, compressed, "disabled: return compressed as-is, no path appended");
}

#[test]
#[serial]
fn over_max_file_bytes_returns_original() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-y");
    let mut cfg = cfg_enabled();
    cfg.max_file_bytes = 100;
    let original = "x".repeat(500); // > 100
    let compressed = "[llm-compress: 略]".to_string();
    let out = attach(compressed, &original, &c, "call1", &cfg);
    assert_eq!(out, original, "exceeds max_file_bytes → return original (preserve lossy recovery capability)");
}

#[test]
#[serial]
fn sanitizes_unsafe_path_components() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    // queryid contains / and .., call_id contains /
    let c = ctx("../../etc/evil");
    let original = "LONG CONTENT ".repeat(50);
    let out = attach("[llm-compress: 略]".to_string(), &original, &c, "a/b/../c", &cfg_enabled());
    let path_part = out.split("原文 ").nth(1).unwrap().trim().trim_end_matches(']').trim();
    // Path must stay within HOME/.codex/llm-compress/ccr, no traversal
    let root = tmp.path().join(".codex/llm-compress/ccr");
    let canon = std::fs::canonicalize(path_part).unwrap();
    assert!(canon.starts_with(std::fs::canonicalize(&root).unwrap()), "path must not escape ccr root");
}

/// Core invariant regression: with short original + long path,
/// attach must return original, never produce "lossy without path" marker.
///
/// Construction: original ~50 bytes (passes max_file_bytes but very short),
/// compressed is "[c]" (3 bytes), so attached = "[c] [llm-compress: 原文 <path>]" easily exceeds original length.
/// Assert: result is either == original or contains "[llm-compress: 原文 " — must not contain "见 ccr".
#[test]
#[serial]
fn short_original_never_emits_pathless_marker() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-short");
    // Original is short enough: ~50 bytes. tmp path typically 50+ bytes, plus "[c] [llm-compress: 原文 …]" prefix must exceed length.
    let original = "short original text for ccr test!!!!!";
    let compressed = "[c]".to_string();
    let cfg = cfg_enabled();
    let out = attach(compressed, original, &c, "call-short", &cfg);
    // Core invariant: result must be either "with path placeholder" or "original", never "lossy without path"
    assert!(
        out == original || out.contains("[llm-compress: 原文 "),
        "violates core invariant: result is neither original nor has path placeholder => {:?}",
        out
    );
    // Specific regression assert: must not emit pathless marker
    assert!(!out.contains("見 ccr"), "violates core invariant: emitted pathless marker `見 ccr` => {:?}", out);
}

#[test]
#[serial]
fn same_fragment_reuses_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-z");
    let original = "REPEATED CONTENT ".repeat(50);
    let o1 = attach("[c1]".to_string(), &original, &c, "call1", &cfg_enabled());
    let o2 = attach("[c2]".to_string(), &original, &c, "call1", &cfg_enabled());
    // Same (call_id, fragment_hash) → same file path
    let p1 = o1.split("原文 ").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let p2 = o2.split("原文 ").nth(1).unwrap().trim().trim_end_matches(']').trim();
    assert_eq!(p1, p2);
}
