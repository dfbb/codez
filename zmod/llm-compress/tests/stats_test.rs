//! stats.rs tests: inject path via tempfile, fix timestamp string,
//! verify exact line format, append semantics, auto-create parent directory, CSV four columns without quotes and no header.

use codez_llm_compress::stats::log_compression_to;
use std::fs;

#[test]
fn writes_exact_single_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    // Read back and assert exact equality (including trailing newline)
    assert_eq!(content, "2026-06-20T08:15:30Z,abc,100,40\n");
}

#[test]
fn appends_second_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();
    log_compression_to(&path, "2026-06-20T09:00:00Z", "def", 200, 80).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "2026-06-20T08:15:30Z,abc,100,40");
    assert_eq!(lines[1], "2026-06-20T09:00:00Z,def,200,80");
}

#[test]
fn creates_missing_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    // A nonexistent subdirectory under tempdir
    let path = dir.path().join("nested").join("deeper").join("llm-compress.log");
    assert!(!path.parent().unwrap().exists());

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();

    assert!(path.exists());
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "2026-06-20T08:15:30Z,abc,100,40\n");
}

#[test]
fn line_format_is_four_columns_no_header_no_quotes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(
        &path,
        "2026-06-20T08:15:30Z",
        "019e3995-5cd9-75a2-b487-f7959835f69e",
        18432,
        5120,
    )
    .unwrap();

    let content = fs::read_to_string(&path).unwrap();
    // No quotes
    assert!(!content.contains('"'));
    // Single line (no header), comma-separated exactly 4 columns
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1);
    let cols: Vec<&str> = lines[0].split(',').collect();
    assert_eq!(cols.len(), 4);
    assert_eq!(cols[0], "2026-06-20T08:15:30Z");
    assert_eq!(cols[1], "019e3995-5cd9-75a2-b487-f7959835f69e");
    assert_eq!(cols[2], "18432");
    assert_eq!(cols[3], "5120");
}
