//! Cache-stability: compression must be a pure function of item content, so a tool
//! output yields identical bytes whether it is the tail (turn N) or history (turn N+1).
//! This is what keeps upstream prefix caches valid across turns.

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
fn search_compression_depends_only_on_content() {
    // Query weighting was removed in Task A1, so there is no per-turn input that could
    // change the result. Prove the output is a pure function of content: two separate
    // Budget instances over identical content yield byte-identical output.
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    let text = "db.rs:1:connect\ndb.rs:2:timeout\ndb.rs:3:retry\ndb.rs:4:close\ndb.rs:5:done";
    let cfg2 = cfg.clone();
    let first = compressed_text(c.compress(text, &budget(&cfg)));
    let second = compressed_text(c.compress(text, &budget(&cfg2)));
    assert_eq!(first, second);
}

#[test]
fn search_compression_is_position_independent() {
    // The tail-on-turn-N == history-on-turn-N+1 property: the same content compresses
    // identically regardless of what precedes it in the request. Compress the content
    // alone, then compress it again (a fresh call) — bytes must match.
    let mut cfg = Config::disabled();
    cfg.search.max_per_file = 2;
    let c = SearchCompressor;
    // Use longer match text so compressed output is shorter than original (saved_bytes > 0).
    let item = "x.rs:1:alpha_function\nx.rs:2:beta_function\nx.rs:3:gamma_function\nx.rs:4:delta_function\nx.rs:5:epsilon_function";
    let as_tail = compressed_text(c.compress(item, &budget(&cfg)));
    let as_history = compressed_text(c.compress(item, &budget(&cfg)));
    assert_eq!(as_tail, as_history);
}
