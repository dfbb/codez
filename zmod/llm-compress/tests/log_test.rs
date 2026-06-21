use codez_llm_compress::compress::log::LogCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
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
    Budget { cfg, cmd: None, query: &[] }
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
fn template_mining_folds_similar_lines_lossless() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100; // 不触发评分删行,只看模板折叠
    cfg.log.template_min_run = 3;
    let c = LogCompressor;
    // 连续同模板行(仅数字不同)
    let text = "worker 1 done\nworker 2 done\nworker 3 done\nworker 4 done\nworker 5 done";
    if let CompressOutcome::Compressed { text: new, lossy, .. } = c.compress(text, &budget_t08(&cfg)) {
        // 模板折叠不删内容
        assert!(!lossy, "纯模板折叠 → lossy=false");
        assert!(new.contains("[llm-compress: 模板]") || new.contains("模板"));
    } else {
        // 也可能因无收益 Unchanged;但 5 行同模板应有收益
        panic!("expected template fold");
    }
}

#[test]
fn detect_still_recognizes_multiline_logs() {
    let cfg = Config::disabled();
    let c = LogCompressor;
    let text = "2026-06-21T08:15:30 INFO a\n2026-06-21T08:15:31 INFO b\n2026-06-21T08:15:32 INFO c\n2026-06-21T08:15:33 INFO d\n2026-06-21T08:15:34 INFO e\n2026-06-21T08:15:35 INFO f\n2026-06-21T08:15:36 INFO g\n2026-06-21T08:15:37 INFO h";
    assert!(c.detect(text, &budget_t08(&cfg)));
}
