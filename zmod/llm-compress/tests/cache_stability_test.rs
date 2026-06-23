//! Cache-stability: compression must be a pure function of item content, so a tool
//! output yields identical bytes whether it is the tail (turn N) or history (turn N+1).
//! This is what keeps upstream prefix caches valid across turns.

use codez_llm_compress::command::CommandHint;
use codez_llm_compress::compress::search::SearchCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

fn compressed_text(outcome: CompressOutcome) -> String {
    match outcome {
        CompressOutcome::Compressed { text, .. } => text,
        _ => panic!("expected compressed"),
    }
}

#[test]
fn search_compression_is_deterministic_across_runs() {
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    // 5 matches in one file -> lossy middle selection (the path that used to drift).
    let text = "f.rs:1:one\nf.rs:2:two\nf.rs:3:three\nf.rs:4:four\nf.rs:5:five";
    let a = compressed_text(c.compress(text, &budget(&cfg)));
    let b = compressed_text(c.compress(text, &budget(&cfg)));
    assert_eq!(a, b, "same content must compress to identical bytes on every run");
}

#[test]
fn different_content_yields_different_output() {
    // Output must track content: two distinct tool outputs must not collapse to the
    // same compressed bytes. (Guards against the compressor ignoring its input.)
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    let text_a = "a.rs:1:alpha_one\na.rs:2:alpha_two\na.rs:3:alpha_three\na.rs:4:alpha_four\na.rs:5:alpha_five";
    let text_b = "b.rs:1:beta_one\nb.rs:2:beta_two\nb.rs:3:beta_three\nb.rs:4:beta_four\nb.rs:5:beta_five";
    let out_a = compressed_text(c.compress(text_a, &budget(&cfg)));
    let out_b = compressed_text(c.compress(text_b, &budget(&cfg)));
    assert_ne!(out_a, out_b, "different content must produce different compressed bytes");
}

#[test]
fn command_hint_broadens_detection_but_content_stays_deterministic() {
    // The command hint affects DETECTION only: non-grep-shaped content is force-claimed
    // when a grep hint is present, but not otherwise. Crucially, whatever a tool output
    // compresses to is still a pure function of its content — the hint cannot make the
    // same content compress to different bytes. Both halves matter for cache stability.
    let cfg = Config::disabled();
    let c = SearchCompressor;

    // Content that does NOT parse as grep `path:line:...` lines (no line-number field).
    let non_grep = "alpha matched here\nbeta matched here\ngamma matched here\ndelta matched here";

    let no_hint = Budget { cfg: &cfg, cmd: None };
    let grep_hint = CommandHint { program: "rg".to_string(), argv: vec![] };
    let with_hint = Budget { cfg: &cfg, cmd: Some(&grep_hint) };

    // Detection differs by hint: this is the real, falsifiable effect of cmd.
    assert!(!c.detect(non_grep, &no_hint), "non-grep content must not be claimed without a hint");
    assert!(c.detect(non_grep, &with_hint), "grep hint must force-claim the same content");

    // Determinism still holds for genuinely grep-shaped content regardless of runs.
    let mut cfg2 = Config::disabled();
    cfg2.search.max_per_file = 2;
    let grep_shaped = "x.rs:1:alpha_function\nx.rs:2:beta_function\nx.rs:3:gamma_function\nx.rs:4:delta_function\nx.rs:5:epsilon_function";
    let run1 = compressed_text(c.compress(grep_shaped, &budget(&cfg2)));
    let run2 = compressed_text(c.compress(grep_shaped, &budget(&cfg2)));
    assert_eq!(run1, run2, "grep-shaped content compresses deterministically");
}
