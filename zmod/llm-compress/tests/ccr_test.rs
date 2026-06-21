use codez_llm_compress::ccr::{attach, CcrRegistry, RequestCtx};
use codez_llm_compress::config::CcrCfg;
use serial_test::serial;
use std::cell::RefCell;
use std::collections::HashMap;

fn ctx<'a>(queryid: &'a str) -> RequestCtx<'a> {
    RequestCtx {
        queryid,
        query_terms: Vec::new(),
        cmd_index: HashMap::new(),
        ccr: RefCell::new(CcrRegistry::new()),
    }
}

fn cfg_enabled() -> CcrCfg {
    CcrCfg { enabled: true, max_files_per_thread: 200, max_thread_bytes: 67_108_864, max_file_bytes: 4_194_304 }
}

#[test]
#[serial]
fn enabled_writes_file_and_appends_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-abc");
    let original = "VERY LONG ORIGINAL CONTENT ".repeat(50);
    let compressed = "[llm-compress: 略 49 行]".to_string();
    let out = attach(compressed.clone(), &original, &c, "call1", &cfg_enabled());
    // 占位里追加了原文路径
    assert!(out.contains("原文:"), "含路径占位");
    assert!(out.starts_with("[llm-compress: 略 49 行]"));
    // 路径指向的文件内容 == 原文
    let path_part = out.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let written = std::fs::read_to_string(path_part).unwrap();
    assert_eq!(written, original);
}

#[test]
#[serial]
fn disabled_returns_compressed_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-x");
    let mut cfg = cfg_enabled();
    cfg.enabled = false;
    let compressed = "[llm-compress: 略 N 行]".to_string();
    let out = attach(compressed.clone(), "original", &c, "call1", &cfg);
    assert_eq!(out, compressed, "disabled:原样返回 compressed,不加路径");
}

#[test]
#[serial]
fn over_max_file_bytes_returns_original() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-y");
    let mut cfg = cfg_enabled();
    cfg.max_file_bytes = 100;
    let original = "x".repeat(500); // > 100
    let compressed = "[llm-compress: 略]".to_string();
    let out = attach(compressed, &original, &c, "call1", &cfg);
    assert_eq!(out, original, "超 max_file_bytes → 返回原文(保有损必可取回)");
}

#[test]
#[serial]
fn sanitizes_unsafe_path_components() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    // queryid 含 / 和 ..,call_id 含 /
    let c = ctx("../../etc/evil");
    let original = "LONG CONTENT ".repeat(50);
    let out = attach("[llm-compress: 略]".to_string(), &original, &c, "a/b/../c", &cfg_enabled());
    let path_part = out.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    // 路径必须落在 HOME/.codex/llm-compress/ccr 下,无穿越
    let root = tmp.path().join(".codex/llm-compress/ccr");
    let canon = std::fs::canonicalize(path_part).unwrap();
    assert!(canon.starts_with(std::fs::canonicalize(&root).unwrap()), "路径不得穿越到 ccr 根外");
}

/// 核心总则回归:短原文 + 长路径场景下,attach 必须返回原文,
/// 绝不产出"有损但无路径"的 `见 ccr` 字符串。
///
/// 构造方式:原文约 50 字节 (足以通过 max_file_bytes 检查但很短),
/// compressed 为 "[c]" (3 字节),使 attached = "[c] [原文: <path>]" 极易超原文长度。
/// 断言:结果要么 == original,要么包含 "原文:" — 不得含 "见 ccr"。
#[test]
#[serial]
fn short_original_never_emits_pathless_marker() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-short");
    // 原文足够短:~50 字节。tmp 路径通常 50+ 字节,加上 "[c] [原文: …]" 前缀必然超长。
    let original = "short original text for ccr test!!!!!";
    let compressed = "[c]".to_string();
    let cfg = cfg_enabled();
    let out = attach(compressed, original, &c, "call-short", &cfg);
    // 核心总则:结果只允许是"含路径占位"或"原文",绝无"有损无路径"
    assert!(
        out == original || out.contains("原文:"),
        "违反核心总则:结果既非原文又无路径占位 => {:?}",
        out
    );
    // 具体回归断言:不得出现无路径 marker
    assert!(!out.contains("见 ccr"), "违反核心总则:产出无路径 marker `见 ccr` => {:?}", out);
}

#[test]
#[serial]
fn same_fragment_reuses_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-z");
    let original = "REPEATED CONTENT ".repeat(50);
    let o1 = attach("[c1]".to_string(), &original, &c, "call1", &cfg_enabled());
    let o2 = attach("[c2]".to_string(), &original, &c, "call1", &cfg_enabled());
    // 同 (call_id, fragment_hash) → 同一文件路径
    let p1 = o1.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let p2 = o2.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    assert_eq!(p1, p2);
}
