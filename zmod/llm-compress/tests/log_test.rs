use codez_llm_compress::compress::log::LogCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

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

#[test]
fn dedup_collapses_consecutive_repeats() {
    let c = LogCompressor;
    let cfg = Config::disabled(); // dedup_repeats 默认 true
    assert!(cfg.log.dedup_repeats);
    let mut s = String::new();
    s.push_str("start\n");
    for _ in 0..5 {
        s.push_str("retrying connection...\n");
    }
    s.push_str("done\n");
    match c.compress(&s, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes, .. } => {
            assert!(text.contains("retrying connection..."));
            assert!(
                text.contains("[llm-compress: 上一行 ×5]"),
                "应折叠为 ×5,实际:\n{text}"
            );
            // 折叠后 retrying 只出现一次正文 + 一行占位。
            assert_eq!(text.matches("retrying connection...").count(), 1);
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("应折叠重复行"),
    }
}

#[test]
fn dedup_disabled_keeps_repeats() {
    let c = LogCompressor;
    // 用 disabled() 再改字段构造 dedup_repeats=false 的 Config。
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false;
    // 给足 head/tail 余量,避免触发截断,纯验证 dedup 不发生。
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    // 带尾换行的多行输入,覆盖尾换行场景。
    let mut s = String::new();
    for _ in 0..6 {
        s.push_str("retrying connection...\n");
    }
    // dedup 关闭、行数 ≤ head+tail,无实质折叠 → 应 Unchanged。
    assert!(
        matches!(c.compress(&s, &budget(&cfg)), CompressOutcome::Unchanged),
        "dedup 关闭且未超 head+tail 时,不应因尾换行副作用误报 Compressed"
    );
}

#[test]
fn head_tail_truncates_long_log_with_placeholder() {
    let c = LogCompressor;
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false; // 隔离 head/tail 行为(各行互不相同)
    cfg.truncate.head_lines = 3;
    cfg.truncate.tail_lines = 3;
    let log = timestamped_log(50); // 50 行,各不相同
    match c.compress(&log, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes, .. } => {
            // 中间被省略:50 - 3 - 3 = 44 行。
            assert!(
                text.contains("[llm-compress: 略 44 行]"),
                "应有 head/tail 占位,实际:\n{text}"
            );
            // 产物行数 = 3 + 1(占位) + 3 = 7 行。
            assert_eq!(text.lines().count(), 7);
            // 保留首行与末行。
            assert!(text.lines().next().unwrap().contains("id=0"));
            assert!(text.lines().last().unwrap().contains("id=49"));
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("长日志应被截断"),
    }
}

#[test]
fn dedup_then_head_tail_combined() {
    let c = LogCompressor;
    let mut cfg = Config::disabled(); // dedup_repeats=true
    cfg.truncate.head_lines = 2;
    cfg.truncate.tail_lines = 2;
    let mut s = String::new();
    s.push_str("2026-06-20T12:00:00 INFO boot\n");
    for _ in 0..30 {
        s.push_str("2026-06-20T12:00:01 WARN retrying...\n");
    }
    s.push_str("2026-06-20T12:00:02 INFO ok line a\n");
    s.push_str("2026-06-20T12:00:03 INFO ok line b\n");
    s.push_str("2026-06-20T12:00:04 INFO ok line c\n");
    match c.compress(&s, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes, .. } => {
            // dedup 后行数 = boot(1) + retrying(1) + 占位(1) + 3 行 ok = 6 行;
            // 6 > head(2)+tail(2)=4 → 仍会再截断为 head(2) + 占位(1) + tail(2)。
            // 此时 dedup 占位可能被 head/tail 的中间省略所吞;产物只有 head/tail 占位。
            assert!(text.contains("[llm-compress: 略"));
            // 验证产物行数 = 2 + 1(占位) + 2 = 5 行。
            assert_eq!(text.lines().count(), 5);
            // boot 行应保留(head 的第一行);ok line c 应保留(tail 的最后一行)。
            assert!(text.contains("boot"));
            assert!(text.contains("ok line c"));
            assert!(saved_bytes > 0);
        }
        CompressOutcome::Unchanged => panic!("应有压缩"),
    }
}

#[test]
fn dedup_two_line_repeat_compresses() {
    // 守卫 count=2 不再漏压缩:第 3、4 行完全相同,其余行各不同。
    // 带时间戳确保 detect 通过,共 10 行 ≥ MIN_LINES(8)。
    let c = LogCompressor;
    let mut cfg = Config::disabled(); // dedup_repeats=true
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    let mut s = String::new();
    s.push_str("2026-06-20T12:00:00 INFO start\n");
    s.push_str("2026-06-20T12:00:01 INFO line 1\n");
    // count=2 重复行:
    s.push_str("2026-06-20T12:00:02 WARN duplicate\n");
    s.push_str("2026-06-20T12:00:02 WARN duplicate\n");
    s.push_str("2026-06-20T12:00:03 INFO line 4\n");
    s.push_str("2026-06-20T12:00:04 INFO line 5\n");
    s.push_str("2026-06-20T12:00:05 INFO line 6\n");
    s.push_str("2026-06-20T12:00:06 INFO line 7\n");
    s.push_str("2026-06-20T12:00:07 INFO line 8\n");
    s.push_str("2026-06-20T12:00:08 INFO done\n");
    match c.compress(&s, &budget(&cfg)) {
        CompressOutcome::Compressed { text, saved_bytes, .. } => {
            assert!(
                text.contains("[llm-compress: 上一行 ×2]"),
                "count=2 重复行应折叠为 ×2 占位,实际:\n{text}"
            );
            assert!(saved_bytes > 0, "应有节省字节数");
        }
        CompressOutcome::Unchanged => panic!("count=2 重复行不应漏压缩,应返回 Compressed"),
    }
}

#[test]
fn unchanged_when_nothing_to_do() {
    let c = LogCompressor;
    let mut cfg = Config::disabled();
    cfg.log.dedup_repeats = false;
    cfg.truncate.head_lines = 100;
    cfg.truncate.tail_lines = 100;
    // 带尾换行的 10 行日志:各行不同、无重复、未超 head+tail。
    let log = timestamped_log(10);
    // 无实质折叠 → 应 Unchanged,不被尾换行副作用误导。
    assert!(
        matches!(c.compress(&log, &budget(&cfg)), CompressOutcome::Unchanged),
        "无重复行、未超 head+tail 时,不应因尾换行副作用误报 Compressed"
    );
}
