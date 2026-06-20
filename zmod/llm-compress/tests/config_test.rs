use codez_llm_compress::config::{load_from, Config};
use std::io::Write;

fn write_tmp(content: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

#[test]
fn enabled_section_overrides_defaults() {
    let f = write_tmp(
        r#"
[llm_compress]
enabled = true
min_total_bytes = 2048

[llm_compress.truncate]
head_lines = 10
tail_lines = 5
max_bytes = 8192

[llm_compress.json]
max_array_items = 7
max_depth = 4

[llm_compress.diff]
context_lines = 1

[llm_compress.log]
dedup_repeats = false
"#,
    );
    let cfg = load_from(f.path());
    assert!(cfg.enabled);
    assert_eq!(cfg.min_total_bytes, 2048);
    assert_eq!(cfg.per_item_min_bytes, 1024); // 未给 → 默认
    assert_eq!(cfg.truncate.head_lines, 10);
    assert_eq!(cfg.truncate.max_bytes, 8192);
    assert_eq!(cfg.json.max_array_items, 7);
    assert_eq!(cfg.json.max_depth, 4);
    assert_eq!(cfg.diff.context_lines, 1);
    assert!(!cfg.log.dedup_repeats);
}

#[test]
fn missing_section_disables() {
    let f = write_tmp("[some_other]\nx = 1\n");
    let cfg = load_from(f.path());
    assert!(!cfg.enabled);
    assert_eq!(cfg.min_total_bytes, 4096); // 默认
}

#[test]
fn missing_file_disables() {
    let cfg = load_from(std::path::Path::new("/nonexistent/zzz/config-zmod.toml"));
    assert!(!cfg.enabled);
}

#[test]
fn malformed_toml_disables() {
    let f = write_tmp("[llm_compress]\nenabled = = true\n");
    let cfg = load_from(f.path());
    assert!(!cfg.enabled);
}

#[test]
fn default_config_is_disabled_with_known_thresholds() {
    let cfg = Config::disabled();
    assert!(!cfg.enabled);
    assert_eq!(cfg.per_item_min_bytes, 1024);
    assert_eq!(cfg.truncate.tail_lines, 50);
    assert!(cfg.log.dedup_repeats);
}
