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
fn compression_independent_of_command_hint() {
    // Per-item independence: a tool output compresses to the same bytes regardless of
    // whether a command hint accompanies it. The command hint can only *broaden*
    // detection (force-claim), never change the bytes the search compressor emits for
    // content it already claims. This is the property that lets a tool output keep its
    // compressed form across turns no matter what else is in the request.
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    let item = "x.rs:1:alpha_function\nx.rs:2:beta_function\nx.rs:3:gamma_function\nx.rs:4:delta_function\nx.rs:5:epsilon_function";

    let no_hint = Budget { cfg: &cfg, cmd: None };
    let hint = CommandHint {
        program: "rg".to_string(),
        argv: vec![],
    };
    let with_hint = Budget { cfg: &cfg, cmd: Some(&hint) };

    let without = compressed_text(c.compress(item, &no_hint));
    let with = compressed_text(c.compress(item, &with_hint));
    assert_eq!(without, with, "command hint must not change compressed bytes for already-claimed content");
}
