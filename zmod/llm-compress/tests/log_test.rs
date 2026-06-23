use codez_llm_compress::compress::log::LogCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

/// Construct a realistic multi-line log with timestamps (≥8 lines).
fn timestamped_log(lines: usize) -> String {
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(&format!(
            "2026-06-20T12:00:{:02} INFO  request handled id={}\n",
            i % 60,
            i
        ));
    }
    s
}

#[test]
fn detect_true_for_timestamped_multiline_log() {
    let c = LogCompressor;
    let cfg = Config::disabled();
    let b = budget(&cfg);
    let log = timestamped_log(12);
    assert!(c.detect(&log, &b), "timestamped multi-line logs should be detected");
}

#[test]
fn detect_true_for_stacktrace() {
    let c = LogCompressor;
    let cfg = Config::disabled();
    let b = budget(&cfg);
    let trace = "\
thread 'main' panicked at 'boom'
stack backtrace:
   0: core::panicking::panic
   1: app::run
             at src/main.rs:42
   2: app::main
             at src/main.rs:10
   3: std::rt::lang_start
   4: main
   5: __libc_start_main";
    assert!(c.detect(trace, &b), "stack traces with `at file:line` should be detected");
}

#[test]
fn detect_false_for_plain_short_text() {
    let c = LogCompressor;
    let cfg = Config::disabled();
    let b = budget(&cfg);
    let txt = "Hello world.\nThis is a short note.\nNothing log-like here.";
    assert!(!c.detect(txt, &b), "plain short text should not be detected");
}

#[test]
fn detect_false_for_long_plain_text_without_log_features() {
    // ≥8 lines but with no log features / no consecutive repeats → should not be detected.
    let c = LogCompressor;
    let cfg = Config::disabled();
    let b = budget(&cfg);
    let mut s = String::new();
    for i in 0..12 {
        s.push_str(&format!("paragraph line number {i} talking about cats\n"));
    }
    assert!(!c.detect(&s, &b), "multi-line plain text without log features should not be detected");
}

#[test]
fn detect_true_for_consecutive_repeats() {
    let c = LogCompressor;
    let cfg = Config::disabled();
    let b = budget(&cfg);
    let mut s = String::new();
    for _ in 0..10 {
        s.push_str("retrying connection...\n");
    }
    assert!(c.detect(&s, &b), "consecutive repeating lines should be detected");
}

// ========== Task 08 additions ==========

fn budget_t08(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

#[test]
fn middle_error_is_kept_not_folded() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    let c = LogCompressor;
    // One ERROR in the middle surrounded by many INFO lines
    let mut lines = Vec::new();
    for i in 0..10 {
        lines.push(format!("INFO step {i}"));
    }
    lines.push("ERROR critical failure at core".to_string());
    for i in 0..10 {
        lines.push(format!("INFO step {}", 10 + i));
    }
    let text = lines.join("\n");
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(&text, &budget_t08(&cfg)) {
        assert!(lossy, "INFO lines should be removed");
        assert_eq!(kind, ContentKind::Text);
        assert!(new.contains("ERROR critical failure"), "ERROR in the middle must be preserved");
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn detect_still_recognizes_multiline_logs() {
    let cfg = Config::disabled();
    let c = LogCompressor;
    let text = "2026-06-21T08:15:30 INFO a\n2026-06-21T08:15:31 INFO b\n2026-06-21T08:15:32 INFO c\n2026-06-21T08:15:33 INFO d\n2026-06-21T08:15:34 INFO e\n2026-06-21T08:15:35 INFO f\n2026-06-21T08:15:36 INFO g\n2026-06-21T08:15:37 INFO h";
    assert!(c.detect(text, &budget_t08(&cfg)));
}

/// Supplementary invariant test: any Compressed outcome's new_text must be strictly shorter than the original.
/// Use a large repetitive log that will be effectively compressed to verify that saved_bytes truly reflects the savings.
#[test]
fn compressed_outcome_always_has_positive_savings() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 3;
    cfg.truncate.tail_lines = 3;
    let c = LogCompressor;

    // Many repetitive lines to ensure real savings
    let mut lines = Vec::new();
    for i in 0..30 {
        lines.push(format!("2026-06-21T10:00:{:02} INFO processed request id={} status=200 path=/api/v1/users", i % 60, i));
    }
    let text = lines.join("\n");

    if let CompressOutcome::Compressed { saved_bytes, text: new_text, .. } = c.compress(&text, &budget_t08(&cfg)) {
        assert!(
            saved_bytes > 0,
            "Compressed must have saved_bytes > 0, actual={}",
            saved_bytes
        );
        assert!(
            new_text.len() < text.len(),
            "Compressed new_text({}) must be strictly shorter than original({})",
            new_text.len(),
            text.len()
        );
    }
    // Unchanged is also valid, no assertion needed
}

// ========== Item D: keep_levels wiring test ==========

/// keep_levels defaults to containing "warn": WARN lines in the middle must be preserved (even if line_score < 1.0).
/// RED before: score_keep ignores keep_levels → WARN line score=0.5 < 1.0 → deleted → test fails.
/// GREEN after: keep_levels wired → WARN line is kept → test passes.
#[test]
fn warn_kept_by_default_keep_levels() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    // default keep_levels = ["error", "warn"]
    let c = LogCompressor;

    // Construct: 2 head INFO lines, 1 WARN in the middle, many INFO lines at tail
    let mut lines: Vec<String> = Vec::new();
    for i in 0..2 {
        lines.push(format!("INFO head {i}"));
    }
    lines.push("WARN disk usage high: 85%".to_string());
    for i in 0..8 {
        lines.push(format!("INFO middle filler {i}"));
    }
    for i in 0..2 {
        lines.push(format!("INFO tail {i}"));
    }
    let text = lines.join("\n");

    match c.compress(&text, &budget_t08(&cfg)) {
        CompressOutcome::Compressed { text: new, .. } => {
            assert!(new.contains("WARN disk usage high"), "WARN line must be preserved by keep_levels");
        }
        CompressOutcome::Unchanged => {
            panic!("expected Compressed (INFO lines should be deleted), got Unchanged");
        }
    }
}

/// When keep_levels=["error"] (without warn): WARN line in the middle can be dropped.
/// Proves that keep_levels truly drives the preservation logic (not hardcoded).
#[test]
fn warn_dropped_when_not_in_keep_levels() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    cfg.log.keep_levels = vec!["error".to_string()]; // warn is not included
    let c = LogCompressor;

    let mut lines: Vec<String> = Vec::new();
    for i in 0..2 {
        lines.push(format!("INFO head {i}"));
    }
    lines.push("WARN disk usage high: 85%".to_string());
    for i in 0..8 {
        lines.push(format!("INFO middle filler {i}"));
    }
    for i in 0..2 {
        lines.push(format!("INFO tail {i}"));
    }
    let text = lines.join("\n");

    match c.compress(&text, &budget_t08(&cfg)) {
        CompressOutcome::Compressed { text: new, .. } => {
            // WARN line not in keep_levels and score < 1.0 → should be deleted
            assert!(!new.contains("WARN disk usage high"), "WARN line should be deleted when warn is not in keep_levels");
        }
        CompressOutcome::Unchanged => {
            panic!("expected Compressed (lines should be deleted), got Unchanged");
        }
    }
}
