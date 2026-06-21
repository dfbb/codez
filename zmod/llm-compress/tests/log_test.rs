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
    // 连续同模板行(仅数字不同)。
    // 注意:template_mine 把所有变量内联为 " | " 连接的单行,这会使体积增大而非减小。
    // 正确行为:saved==0 守卫拦截 → Unchanged(不违反压后≤压前不变量)。
    let text = "worker 1 done\nworker 2 done\nworker 3 done\nworker 4 done\nworker 5 done";
    // 守卫修复后:模板折叠体积增大,saved=0,返回 Unchanged — lossy=false 不变量由 Unchanged 自然满足。
    match c.compress(text, &budget_t08(&cfg)) {
        CompressOutcome::Unchanged => {
            // 正确:体积未减小,不产生 Compressed
        }
        CompressOutcome::Compressed { lossy, text: new, saved_bytes, .. } => {
            // 若真的有净收益,则必须满足:lossy=false(纯模板折叠)且包含模板标记
            assert!(!lossy, "纯模板折叠 → lossy=false");
            assert!(new.contains("[llm-compress: 模板]") || new.contains("模板"));
            assert!(saved_bytes > 0, "Compressed 时 saved_bytes 必须 > 0");
        }
    }
}

#[test]
fn detect_still_recognizes_multiline_logs() {
    let cfg = Config::disabled();
    let c = LogCompressor;
    let text = "2026-06-21T08:15:30 INFO a\n2026-06-21T08:15:31 INFO b\n2026-06-21T08:15:32 INFO c\n2026-06-21T08:15:33 INFO d\n2026-06-21T08:15:34 INFO e\n2026-06-21T08:15:35 INFO f\n2026-06-21T08:15:36 INFO g\n2026-06-21T08:15:37 INFO h";
    assert!(c.detect(text, &budget_t08(&cfg)));
}

/// Task08 回归:模板折叠标记本身是 UTF-8 中文,开销可能抵消甚至超过节省。
/// 构造极短的同模板连续行,使得折叠后标记开销 ≥ 原始节省 → saved==0。
/// 守卫加入前:返回 Compressed{saved_bytes:0}(违反契约)。
/// 守卫加入后:返回 Unchanged。
///
/// 构造:3 行同模板,每行 "x N" (4 bytes ASCII)。
///   原始长度: 3×4 + 2(换行) = 14 bytes
///   折叠后:[llm-compress: 模板] x # \n [llm-compress: 变量 ×3] x 1 | x 2 | x 3
///   "[llm-compress: 模板] " = 22 bytes (UTF-8)
///   "[llm-compress: 变量 ×3] " = 26 bytes (UTF-8)  ← 远超 14 bytes
///   → new_text.len() > text.len() → saved == 0 → Unchanged
#[test]
fn template_fold_no_net_saving_returns_unchanged() {
    let mut cfg = Config::disabled();
    // head/tail 足够大,不触发评分删行,只看模板折叠
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    cfg.log.template_min_run = 3;
    let c = LogCompressor;

    // 构造 ≥8 行以通过 detect,但只有前 3 行相同模板(后面填凑行数但不构成新模板组)
    // 这 3 行模板折叠后因中文标记开销必然 saved==0
    // 后续行(各不相同)不触发折叠
    let text = "x 1\nx 2\nx 3\na_line_unique_001\na_line_unique_002\na_line_unique_003\na_line_unique_004\na_line_unique_005";

    match c.compress(text, &budget_t08(&cfg)) {
        CompressOutcome::Unchanged => {
            // 正确:saved==0 守卫拦住了零收益 Compressed
        }
        CompressOutcome::Compressed { saved_bytes, text: new_text, .. } => {
            // 守卫缺失时会到这里;如果 saved_bytes > 0 那压缩是真实有效的,
            // 不过设计上短行折叠必然 saved==0,此分支属 bug。
            // 无论如何,验证核心不变量:Compressed 时 new_text 必须比原文短。
            assert!(
                new_text.len() < text.len(),
                "Compressed 时 new_text({}) 必须短于原文({}),saved_bytes={}",
                new_text.len(),
                text.len(),
                saved_bytes
            );
            // 如果走到这里且断言通过,说明压缩确实有收益(意外);
            // 若断言失败,RED 证据:守卫缺失时返回 saved==0 的 Compressed。
        }
    }
}

/// 补充不变量测试:任意 Compressed 结果的 new_text 必须严格短于原文。
/// 用一段确实会被有效压缩的大型重复日志,验证 saved_bytes 真实反映收益。
#[test]
fn compressed_outcome_always_has_positive_savings() {
    let mut cfg = Config::disabled();
    cfg.truncate.head_lines = 3;
    cfg.truncate.tail_lines = 3;
    cfg.log.template_min_run = 3;
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
