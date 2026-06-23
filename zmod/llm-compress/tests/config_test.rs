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

[llm_compress.truncate]
head_lines = 10
tail_lines = 5
max_bytes = 8192

[llm_compress.diff]
context_lines = 1
"#,
    );
    let cfg = load_from(f.path());
    assert!(cfg.enabled);
    assert_eq!(cfg.per_item_min_bytes, 1024); // Not provided → defaults
    assert_eq!(cfg.truncate.head_lines, 10);
    assert_eq!(cfg.truncate.max_bytes, 8192);
    assert_eq!(cfg.diff.context_lines, 1);
}

#[test]
fn missing_section_disables() {
    let f = write_tmp("[some_other]\nx = 1\n");
    let cfg = load_from(f.path());
    assert!(!cfg.enabled);
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
}

#[test]
fn use_toon_defaults_true_and_parses_false() {
    // Default (field absent) must be true.
    let cfg = Config::disabled();
    assert!(cfg.json.use_toon, "use_toon must default to true");

    // Explicit false in config must parse as false.
    let f = write_tmp(
        "[llm_compress]\nenabled = true\n\n[llm_compress.json]\nuse_toon = false\n",
    );
    let parsed = load_from(f.path());
    assert!(!parsed.json.use_toon, "use_toon = false must parse");
}
