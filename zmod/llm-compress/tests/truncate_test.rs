use codez_llm_compress::compress::truncate::TruncateCompressor;
use codez_llm_compress::config::{Config, TruncateCfg};
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

/// 用指定 truncate 阈值构造一个 Config(其余字段取默认)。
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
    // 3 行,远低于 head+tail=100,字节也远低于 max_bytes。
    let cfg = cfg_with(50, 50, 16384);
    let input = "line1\nline2\nline3";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    assert!(matches!(out, CompressOutcome::Unchanged));
}

#[test]
fn large_input_keeps_head_and_tail_with_marker() {
    // head=2, tail=2;造 200 行较长行 → 省略 196 行远大于标记 → 真实压缩。
    let cfg = cfg_with(2, 2, 1_000_000); // max_bytes 极大,不触发硬截断
    let lines: Vec<String> = (0..200).map(|i| format!("line{i:04}_payload_xxxxxxxxxxxxxxxxxxxx")).collect();
    let input = lines.join("\n");
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes, .. } = out else {
        panic!("expected Compressed");
    };
    // 头 2 行在;
    assert!(text.contains("line0000"));
    assert!(text.contains("line0001"));
    // 尾 2 行在;
    assert!(text.contains("line0198"));
    assert!(text.contains("line0199"));
    // 中间省略标记(非硬截断标记);
    assert!(text.contains("[llm-compress: 略"));
    assert!(text.contains("行 /"));
    assert!(saved_bytes > 0);
}

#[test]
fn ansi_escapes_are_stripped() {
    // 含颜色码;阈值放大到不会触发行/字节截断,只验证 ANSI 被剥离。
    // 注意:剥 ANSI 后仍是小输入 → Unchanged,故这里用一行超 max_bytes 的方式逼出压缩,
    // 但为聚焦 ANSI,改用 head=0/tail=0 + 一个会被硬截断的长行也不便观察。
    // 采用:多行 + 低 head/tail,使其进入压缩路径,再断言输出里无 \x1b。
    let cfg = cfg_with(1, 1, 16384);
    let input = "\x1b[31mred0\x1b[0m\n\x1b[1;32mgreen1\x1b[0m\n\x1b[33myellow2\x1b[0m\n\x1b[34mblue3\x1b[0m";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, .. } = out else {
        panic!("expected Compressed");
    };
    // 输出中不得残留任何 ESC 字节;
    assert!(!text.contains('\x1b'));
    // 颜色码被剥离后,纯文本仍在;
    assert!(text.contains("red0"));
    assert!(text.contains("blue3"));
}

#[test]
fn hard_truncate_does_not_split_utf8() {
    // head=tail=0 → 全体进占位;占位标记本身含中文(多字节),
    // 设极小 max_bytes 逼出硬截断,断言输出仍是合法 UTF-8(能成功转回 &str)。
    let cfg = cfg_with(0, 0, 12);
    // 多行中文,字节数远超 12。
    let input = "甲乙丙丁\n戊己庚辛\n壬癸子丑\n寅卯辰巳";
    let out = TruncateCompressor.compress(input, &budget(&cfg));
    let CompressOutcome::Compressed { text, saved_bytes, .. } = out else {
        panic!("expected Compressed");
    };
    // 关键:String 天然保证 UTF-8;若硬截断切坏边界,实现里会 panic 或产出非法字节。
    // 这里再显式校验:bytes 能无损解析回字符串(round-trip)。
    assert_eq!(std::str::from_utf8(text.as_bytes()).unwrap(), text);
    // 硬截断标记存在;
    assert!(text.contains("[llm-compress: 截断至 max_bytes]"));
    assert!(saved_bytes > 0);
}

#[test]
fn saved_bytes_is_original_minus_new() {
    // head=1/tail=1,造 100 行长行 → 省略 98 行远大于标记 → 真实压缩。
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
    // 仅 2 行(≤ head+tail=4)但字节远超 max_bytes → 走硬截断分支。
    // max_bytes 取合理值(2048),正文造 ~8000 字节 → 硬截断后远小于原文。
    let cfg = cfg_with(2, 2, 2048);
    let big_line_a = "A".repeat(4000);
    let big_line_b = "B".repeat(4000);
    let input = format!("{big_line_a}\n{big_line_b}"); // ~8001 字节,2 行
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
    assert!(lossy, "截断删内容 → lossy=true");
    assert_eq!(kind, ContentKind::Text);
}
