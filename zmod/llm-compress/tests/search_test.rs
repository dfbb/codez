use codez_llm_compress::compress::search::SearchCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

#[test]
fn detect_recognizes_grep_lines() {
    let cfg = Config::disabled();
    let c = SearchCompressor;
    let text = "src/a.rs:10:fn foo()\nsrc/a.rs:20:fn bar()\nsrc/b.rs:5:struct X\nsrc/b.rs:8:impl X\nsrc/c.rs:1:use std\nsrc/c.rs:2:use core\nsrc/c.rs:3:mod m";
    assert!(c.detect(text, &budget(&cfg)));
}

#[test]
fn detect_rejects_non_grep() {
    let cfg = Config::disabled();
    let c = SearchCompressor;
    assert!(!c.detect("just\nplain\ntext\nlines\nhere", &budget(&cfg)));
}

#[test]
fn keeps_first_and_last_match_per_file() {
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    // One file with 5 matches
    let text = "f.rs:1:one\nf.rs:2:two\nf.rs:3:three\nf.rs:4:four\nf.rs:5:five";
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &budget(&cfg)) {
        assert!(lossy);
        assert_eq!(kind, ContentKind::Text);
        assert!(new.contains("f.rs:1:one"), "keep first match");
        assert!(new.contains("f.rs:5:five"), "keep last match");
        assert!(new.contains("[llm-compress: 略"));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn files_over_limit_are_folded() {
    let mut cfg = Config::disabled();
    cfg.search.max_files = 2;
    cfg.search.max_per_file = 5;
    let c = SearchCompressor;
    let mut lines = Vec::new();
    for f in 0..5 {
        for l in 0..3 {
            lines.push(format!("file{f}.rs:{l}:content {f} {l}"));
        }
    }
    let text = lines.join("\n");
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(&text, &budget(&cfg)) {
        assert!(new.contains("个文件"), "file count exceeds limit, fold");
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn is_grep_command_forces_detect() {
    // Even if line format is atypical, is_grep match claims it (provided by budget.cmd). This test only verifies that detect reads budget.cmd without panic.
    let cfg = Config::disabled();
    let c = SearchCompressor;
    let hint = codez_llm_compress::command::CommandHint { program: "rg".to_string(), argv: vec![] };
    let b = Budget { cfg: &cfg, cmd: Some(&hint) };
    // Multiple lines but not standard grep format; is_grep match → detect true
    assert!(c.detect("matchy line 1\nmatchy line 2\nmatchy line 3", &b));
}
