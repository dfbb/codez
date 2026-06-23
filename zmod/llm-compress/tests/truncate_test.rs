use codez_llm_compress::compress::truncate::TruncateCompressor;
use codez_llm_compress::config::{Config, TruncateCfg};
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

/// Construct a Config with the specified truncate thresholds (other fields take defaults).
fn cfg_with(head_lines: usize, tail_lines: usize, max_bytes: usize) -> Config {
    let mut c = Config::disabled();
    c.truncate = TruncateCfg { head_lines, tail_lines, max_bytes };
    c
}

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

#[test]
fn name_is_truncate() {
    assert_eq!(TruncateCompressor.name(), "truncate");
}

#[test]
fn detect_is_always_true() {
    let cfg = Config::disabled();
    let b = budget(&cfg);
    assert!(TruncateCompressor.detect("", &b));
    assert!(TruncateCompressor.detect("anything at all", &b));
}

#[test]
fn small_input_is_unchanged() {
    // 3 lines, well below head+tail=100, bytes also well below max_bytes.
    let cfg = cfg_with(50, 50, 16384);
    let input = "line1\nline2\nline3";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    assert!(matches!(out, CompressOutcome::Unchanged));
}

#[test]
fn large_input_keeps_head_and_tail_with_marker() {
    // head=2, tail=2; create 200 long lines → omit 196 lines much greater than marker → real compression.
    let cfg = cfg_with(2, 2, 1_000_000); // max_bytes very large, does not trigger hard truncation
    let lines: Vec<String> = (0..200).map(|i| format!("line{i:04}_payload_xxxxxxxxxxxxxxxxxxxx")).collect();
    let input = lines.join("\n");
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes, .. } = out else {
        panic!("expected Compressed");
    };
    // First 2 lines present;
    assert!(text.contains("line0000"));
    assert!(text.contains("line0001"));
    // Last 2 lines present;
    assert!(text.contains("line0198"));
    assert!(text.contains("line0199"));
    // Middle omission marker (not hard truncation marker);
    assert!(text.contains("[llm-compress: 略"));
    assert!(text.contains("行 /"));
    assert!(saved_bytes > 0);
}

#[test]
fn ansi_escapes_are_stripped() {
    // Contains color codes; raise threshold so line/byte truncation doesn't trigger, only verify ANSI is stripped.
    // Note: after stripping ANSI, still small input → Unchanged, so use multi-line + low head/tail
    // to enter compression path, then assert output has no \x1b.
    let cfg = cfg_with(1, 1, 16384);
    let input = "\x1b[31mred0\x1b[0m\n\x1b[1;32mgreen1\x1b[0m\n\x1b[33myellow2\x1b[0m\n\x1b[34mblue3\x1b[0m";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, .. } = out else {
        panic!("expected Compressed");
    };
    // Output must not contain any ESC byte;
    assert!(!text.contains('\x1b'));
    // After color codes are stripped, plain text remains;
    assert!(text.contains("red0"));
    assert!(text.contains("blue3"));
}

#[test]
fn hard_truncate_does_not_split_utf8() {
    // head=tail=0 → all becomes placeholder; placeholder marker itself contains Chinese (multi-byte),
    // set very small max_bytes to force hard truncation, assert output is still valid UTF-8 (can convert back to &str).
    let cfg = cfg_with(0, 0, 12);
    // Multiple lines of Chinese, byte count far exceeds 12.
    let input = "甲乙丙丁\n戊己庚辛\n壬癸子丑\n寅卯辰巳";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes, .. } = out else {
        panic!("expected Compressed");
    };
    // Key point: String naturally guarantees UTF-8; if hard truncation cuts the boundary,
    // the implementation would panic or produce invalid bytes.
    // Here we explicitly verify: bytes can be losslessly parsed back to string (round-trip).
    assert_eq!(std::str::from_utf8(text.as_bytes()).unwrap(), text);
    // Hard truncation marker present;
    assert!(text.contains("[llm-compress: 截断至 max_bytes]"));
    assert!(saved_bytes > 0);
}

#[test]
fn saved_bytes_is_original_minus_new() {
    // head=1/tail=1, create 100 long lines → omit 98 lines much greater than marker → real compression.
    let cfg = cfg_with(1, 1, 1_000_000);
    let lines: Vec<String> = (0..100).map(|i| format!("row{i:03}_{}", "z".repeat(40))).collect();
    let input = lines.join("\n");
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes, .. } = out else {
        panic!("expected Compressed");
    };
    assert_eq!(saved_bytes, input.len() - text.len());
    assert!(saved_bytes > 0);
}

#[test]
fn over_byte_limit_triggers_hard_truncate() {
    // Only 2 lines (≤ head+tail=4) but bytes far exceed max_bytes → take hard truncation branch.
    // max_bytes set to reasonable value (2048), content creates ~8000 bytes → after hard truncate, much smaller than original.
    let cfg = cfg_with(2, 2, 2048);
    let big_line_a = "A".repeat(4000);
    let big_line_b = "B".repeat(4000);
    let input = format!("{big_line_a}\n{big_line_b}"); // ~8001 bytes, 2 lines
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes, .. } = out else {
        panic!("expected Compressed (byte limit exceeded)");
    };
    assert!(text.contains("[llm-compress: 截断至 max_bytes]"));
    assert!(saved_bytes > 0);
    assert!(text.len() < input.len());
}

#[test]
fn truncate_marks_lossy_text_kind() {
    use codez_llm_compress::router::ContentKind;
    let cfg = cfg_with(2, 2, 1_000_000);
    let lines: Vec<String> = (0..200).map(|i| format!("line{i:04}_payload_xxxxxxxxxxxxxxxxxxxx")).collect();
    let input = lines.join("\n");
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { lossy, kind, .. } = out else {
        panic!("expected Compressed");
    };
    assert!(lossy, "truncation removes content → lossy=true");
    assert_eq!(kind, ContentKind::Text);
}
