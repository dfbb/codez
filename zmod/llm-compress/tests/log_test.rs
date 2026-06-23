use codez_llm_compress::compress::log::LogCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

/// 构造一段真实风格、带时间戳的多行日志(≥8 行)。
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
    assert!(c.detect(&log, &b), "带时间戳的多行日志应被认领");
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
    assert!(c.detect(trace, &b), "含 `at file:line` 的栈跟踪应被认领");
}

#[test]
fn detect_false_for_plain_short_text() {
    let c = LogCompressor;
    let cfg = Config::disabled();
    let b = budget(&cfg);
    let txt = "Hello world.\nThis is a short note.\nNothing log-like here.";
    assert!(!c.detect(txt, &b), "普通短文本不应被认领");
}

#[test]
fn detect_false_for_long_plain_text_without_log_features() {
    // ≥8 行但无任何日志特征 / 无连续重复 → 不认领。
    let c = LogCompressor;
    let cfg = Config::disabled();
    let b = budget(&cfg);
    let mut s = String::new();
    for i in 0..12 {
        s.push_str(&format!("paragraph line number {i} talking about cats\n"));
    }
    assert!(!c.detect(&s, &b), "多行但无日志特征的普通文本不应被认领");
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
    assert!(c.detect(&s, &b), "存在连续重复行应被认领");
}

// ========== Task 08 新增 ==========

fn budget_t08(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

#[test]
fn middle_error_is_kept_not_folded() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    let c = LogCompressor;
    // 中段一条 ERROR,被大量 INFO 包围
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
        assert!(lossy, "删了 INFO 行");
        assert_eq!(kind, ContentKind::Text);
        assert!(new.contains("ERROR critical failure"), "中段 ERROR 必须保留");
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

/// 补充不变量测试:任意 Compressed 结果的 new_text 必须严格短于原文。
/// 用一段确实会被有效压缩的大型重复日志,验证 saved_bytes 真实反映收益。
#[test]
fn compressed_outcome_always_has_positive_savings() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 3;
    cfg.truncate.tail_lines = 3;
    let c = LogCompressor;

    // 大量重复行,确保有真实节省
    let mut lines = Vec::new();
    for i in 0..30 {
        lines.push(format!("2026-06-21T10:00:{:02} INFO processed request id={} status=200 path=/api/v1/users", i % 60, i));
    }
    let text = lines.join("\n");

    if let CompressOutcome::Compressed { saved_bytes, text: new_text, .. } = c.compress(&text, &budget_t08(&cfg)) {
        assert!(
            saved_bytes > 0,
            "Compressed 时 saved_bytes 必须 > 0,实际={}",
            saved_bytes
        );
        assert!(
            new_text.len() < text.len(),
            "Compressed 时 new_text({}) 必须严格短于原文({})",
            new_text.len(),
            text.len()
        );
    }
    // Unchanged 也合法,无需断言
}

// ========== Item D: keep_levels 接线测试 ==========

/// keep_levels 默认含 "warn":中段 WARN 行必须被保留(即使 line_score < 1.0)。
/// RED 前:score_keep 不看 keep_levels → WARN 行 score=0.5 < 1.0 → 被删 → 测试失败。
/// GREEN 后:keep_levels 接线 → WARN 行被留下 → 测试通过。
#[test]
fn warn_kept_by_default_keep_levels() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    // 默认 keep_levels = ["error", "warn"]
    let c = LogCompressor;

    // 构造:头2行 INFO、中段1行 WARN、尾部大量 INFO
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
            assert!(new.contains("WARN disk usage high"), "WARN 行必须被 keep_levels 保留");
        }
        CompressOutcome::Unchanged => {
            panic!("期望 Compressed(有 INFO 行应被删除),实际 Unchanged");
        }
    }
}

/// keep_levels=["error"] 时(不含 warn):中段 WARN 行可以被丢弃。
/// 证明 keep_levels 真正驱动保留逻辑(非硬编码)。
#[test]
fn warn_dropped_when_not_in_keep_levels() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    cfg.log.keep_levels = vec!["error".to_string()]; // warn 不在其中
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
            // WARN 行不在 keep_levels 且 score < 1.0 → 应被删除
            assert!(!new.contains("WARN disk usage high"), "keep_levels 不含 warn 时,WARN 行应被删除");
        }
        CompressOutcome::Unchanged => {
            panic!("期望 Compressed(有行应被删除),实际 Unchanged");
        }
    }
}
