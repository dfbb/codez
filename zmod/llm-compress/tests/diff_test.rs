//! DiffCompressor 集成测试。
//!
//! 覆盖:
//! - detect 对真实 git diff 为 true、对普通文本为 false;
//! - 大段上下文的 hunk 被折叠,变更行全保留;
//! - 占位标记存在;
//! - 小 diff(上下文本就少)→ Unchanged;
//! - saved_bytes 正确。

use codez_llm_compress::compress::diff::DiffCompressor;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use codez_llm_compress::config::Config;

/// 构造一个 context_lines=N 的 Config(借助 Task 01 的默认值再覆盖 diff 字段)。
fn cfg_with_context(n: usize) -> Config {
    let mut cfg = Config::default();
    cfg.diff.context_lines = n;
    cfg
}

/// 一段真实的多行 unified diff fixture:单文件、单 hunk,含大段未变更上下文。
/// hunk 内:6 行上文 + 1 行删除 + 1 行新增 + 6 行下文。
const REAL_DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,14 +1,14 @@
 line ctx 1
 line ctx 2
 line ctx 3
 line ctx 4
 line ctx 5
 line ctx 6
-old changed line
+new changed line
 line ctx 7
 line ctx 8
 line ctx 9
 line ctx 10
 line ctx 11
 line ctx 12
";

#[test]
fn detect_true_for_real_git_diff() {
    let c = DiffCompressor;
    assert!(c.detect(REAL_DIFF), "真实 git diff 应被识别");
}

#[test]
fn detect_true_for_bare_hunk_header() {
    let c = DiffCompressor;
    let text = "@@ -1,3 +1,4 @@\n a\n-b\n+c\n d\n";
    assert!(c.detect(text), "含 hunk 头应被识别");
}

#[test]
fn detect_false_for_plain_text() {
    let c = DiffCompressor;
    let text = "这是一段普通文本。\n没有任何 diff 特征。\n+ 这不是变更行只是个加号开头的句子\n";
    // 注意:仅靠单独的 '+' 开头一行不构成 diff(无 hunk 头、无 diff --git、无 '--- '+'+++ ' 配对)。
    assert!(!c.detect(text), "普通文本不应被识别");
}

#[test]
fn compress_folds_large_context_and_keeps_changes() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(2);
    let budget = Budget { cfg: &cfg };

    let outcome = c.compress(REAL_DIFF, &budget);
    let CompressOutcome::Compressed { text, saved_bytes } = outcome else {
        panic!("大段上下文应被压缩");
    };

    // 变更行必须完整保留。
    assert!(text.contains("-old changed line"), "删除行须保留");
    assert!(text.contains("+new changed line"), "新增行须保留");

    // 文件头与 hunk 头须保留。
    assert!(text.contains("diff --git a/src/lib.rs b/src/lib.rs"));
    assert!(text.contains("index 1234567..89abcde 100644"));
    assert!(text.contains("--- a/src/lib.rs"));
    assert!(text.contains("+++ b/src/lib.rs"));
    assert!(text.contains("@@ -1,14 +1,14 @@"));

    // 紧邻变更行前后各 2 行上下文须保留。
    assert!(text.contains(" line ctx 5"), "变更行前第 2 行须保留");
    assert!(text.contains(" line ctx 6"), "变更行前第 1 行须保留");
    assert!(text.contains(" line ctx 7"), "变更行后第 1 行须保留");
    assert!(text.contains(" line ctx 8"), "变更行后第 2 行须保留");

    // 被折叠的远端上下文不应出现。
    assert!(!text.contains(" line ctx 1"), "远端上文应被折叠");
    assert!(!text.contains(" line ctx 12"), "远端下文应被折叠");

    // 占位标记须存在。
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "上文 6-2=4 行应折叠为占位,实际输出:\n{text}"
    );
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "下文 6-2=4 行应折叠为占位"
    );

    // saved_bytes 应等于原文与压缩后文本的字节差。
    assert_eq!(saved_bytes, REAL_DIFF.len() - text.len(), "saved_bytes 须为字节差");
    assert!(saved_bytes > 0);
}

#[test]
fn compress_small_diff_unchanged() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(3);
    let budget = Budget { cfg: &cfg };

    // 上下文本就 ≤ context_lines,无可折叠。
    let small = "\
diff --git a/a.txt b/a.txt
index aaa..bbb 100644
--- a/a.txt
+++ b/a.txt
@@ -1,4 +1,4 @@
 ctx 1
 ctx 2
-old
+new
 ctx 3
";
    let outcome = c.compress(small, &budget);
    assert!(
        matches!(outcome, CompressOutcome::Unchanged),
        "无可折叠上下文时应 Unchanged"
    );
}
