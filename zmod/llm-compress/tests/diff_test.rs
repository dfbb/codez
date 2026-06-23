//! DiffCompressor integration tests.
//!
//! Coverage:
//! - detect returns true for real git diff, false for plain text;
//! - large context hunks are collapsed, change lines fully preserved;
//! - placeholder markers present;
//! - small diff (little context) → Unchanged;
//! - saved_bytes calculated correctly.

use codez_llm_compress::compress::diff::DiffCompressor;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use codez_llm_compress::config::Config;

/// Construct a Config with context_lines=N (using Task 01 defaults and overriding diff field).
fn cfg_with_context(n: usize) -> Config {
    let mut cfg = Config::default();
    cfg.diff.context_lines = n;
    cfg
}

/// A real multi-line unified diff fixture: single file, single hunk, with large unchanged context.
/// hunk contains: 6 context lines before + 1 deleted line + 1 added line + 6 context lines after.
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
    let cfg = Config::default();
    let budget = Budget { cfg: &cfg, cmd: None };
    assert!(c.detect(REAL_DIFF, &budget), "real git diff should be recognized");
}

#[test]
fn detect_true_for_bare_hunk_header() {
    let c = DiffCompressor;
    let cfg = Config::default();
    let budget = Budget { cfg: &cfg, cmd: None };
    let text = "@@ -1,3 +1,4 @@\n a\n-b\n+c\n d\n";
    assert!(c.detect(text, &budget), "hunk header should be recognized");
}

#[test]
fn detect_false_for_plain_text() {
    let c = DiffCompressor;
    let cfg = Config::default();
    let budget = Budget { cfg: &cfg, cmd: None };
    let text = "这是一段普通文本。\n没有任何 diff 特征。\n+ 这不是变更行只是个加号开头的句子\n";
    // Note: a single line starting with '+' alone does not constitute a diff (no hunk header, no diff --git, no '--- '+'+++ ' pairing).
    assert!(!c.detect(text, &budget), "plain text should not be recognized");
}

#[test]
fn compress_folds_large_context_and_keeps_changes() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(2);
    let budget = Budget { cfg: &cfg, cmd: None };

    let outcome = c.compress(REAL_DIFF, &budget);
    let CompressOutcome::Compressed { text, saved_bytes, .. } = outcome else {
        panic!("large context should be compressed");
    };

    // Change lines must be fully preserved.
    assert!(text.contains("-old changed line"), "deleted line must be preserved");
    assert!(text.contains("+new changed line"), "added line must be preserved");

    // File header and hunk header must be preserved.
    assert!(text.contains("diff --git a/src/lib.rs b/src/lib.rs"));
    assert!(text.contains("index 1234567..89abcde 100644"));
    assert!(text.contains("--- a/src/lib.rs"));
    assert!(text.contains("+++ b/src/lib.rs"));
    assert!(text.contains("@@ -1,14 +1,14 @@"));

    // 2 context lines before and after the change line must be preserved.
    assert!(text.contains(" line ctx 5"), "2nd line before change must be preserved");
    assert!(text.contains(" line ctx 6"), "1st line before change must be preserved");
    assert!(text.contains(" line ctx 7"), "1st line after change must be preserved");
    assert!(text.contains(" line ctx 8"), "2nd line after change must be preserved");

    // Distant context that was folded should not appear.
    assert!(!text.contains(" line ctx 1"), "remote context before should be folded");
    assert!(!text.contains(" line ctx 12"), "remote context after should be folded");

    // Placeholder markers must exist.
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "4 context lines before (6-2=4) should be folded into placeholder, actual output:\n{text}"
    );
    assert!(
        text.contains("[llm-compress: 略 4 行上下文]"),
        "4 context lines after (6-2=4) should be folded into placeholder"
    );

    // saved_bytes should equal the byte difference between original and compressed text.
    assert_eq!(saved_bytes, REAL_DIFF.len() - text.len(), "saved_bytes must be byte difference");
    assert!(saved_bytes > 0);
}

#[test]
fn compress_small_diff_unchanged() {
    let c = DiffCompressor;
    let cfg = cfg_with_context(3);
    let budget = Budget { cfg: &cfg, cmd: None };
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
        "should be Unchanged when no foldable context"
    );
}

#[test]
fn diff_fold_marks_lossy_text_kind() {
    use codez_llm_compress::compress::diff::DiffCompressor;
    use codez_llm_compress::config::Config;
    use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

    let mut cfg = Config::disabled();
    cfg.diff.context_lines = 1;
    let c = DiffCompressor;
    // Construct a diff with excess context that can be folded
    let mut lines = vec!["diff --git a/f b/f".to_string(), "--- a/f".to_string(), "+++ b/f".to_string(), "@@ -1,20 +1,20 @@".to_string()];
    for i in 0..10 {
        lines.push(format!(" ctx{i}"));
    }
    lines.push("-old".to_string());
    lines.push("+new".to_string());
    for i in 0..10 {
        lines.push(format!(" ctx{}", 10 + i));
    }
    let text = lines.join("\n");
    let b = Budget { cfg: &cfg, cmd: None };
    if let CompressOutcome::Compressed { lossy, kind, .. } = c.compress(&text, &b) {
        assert!(lossy, "diff context folding → lossy=true");
        assert_eq!(kind, ContentKind::Text);
    } else {
        panic!("expected compressed diff");
    }
}
