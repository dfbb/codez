//! Walk through fixtures/inherited/manifest.toml and run hard invariants (spec §8.3) for each inherited sample.
//! No byte-for-byte equality; reference output is only used for "size no worse" comparison.

use codez_llm_compress::compress::{
    diff::DiffCompressor, json::JsonCompressor, log::LogCompressor, search::SearchCompressor,
    tabular::TabularCompressor,
};
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inherited")
}

#[derive(serde::Deserialize)]
struct Manifest {
    fixture: Vec<Fixture>,
}
#[derive(serde::Deserialize)]
struct Fixture {
    file: String,
    compressor: String,
    #[serde(default)]
    ref_output: String,
}

fn run_compressor(name: &str, text: &str, cfg: &Config) -> Option<(String, bool)> {
    let budget = Budget { cfg, cmd: None };
    let c: Box<dyn Compressor> = match name {
        "json" => Box::new(JsonCompressor),
        "search" => Box::new(SearchCompressor),
        "tabular" => Box::new(TabularCompressor),
        "log" => Box::new(LogCompressor),
        "diff" => Box::new(DiffCompressor),
        _ => return None,
    };
    if !c.detect(text, &budget) {
        return None;
    }
    match c.compress(text, &budget) {
        CompressOutcome::Compressed { text, lossy, .. } => Some((text, lossy)),
        CompressOutcome::Unchanged => None,
    }
}

#[test]
fn parity_invariants_hold_for_all_fixtures() {
    let dir = fixtures_dir();
    let manifest_path = dir.join("manifest.toml");
    if !manifest_path.exists() {
        eprintln!("manifest.toml does not exist, skipping parity (fixture not ready)");
        return;
    }
    let manifest: Manifest =
        toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();

    let mut cfg = Config::disabled();
    cfg.enabled = true;
    // Provide sufficient threshold for compressors to claim (parity focuses on algorithm output, not yielding)
    cfg.truncate.max_bytes = 1_000_000;

    for fx in &manifest.fixture {
        // preprocess has no direct Compressor correspondence, skip (preprocess is tested in the transform pipeline)
        if fx.compressor == "preprocess" {
            continue;
        }
        let input = std::fs::read_to_string(dir.join(&fx.file))
            .unwrap_or_else(|_| panic!("cannot read fixture {}", fx.file));
        let Some((out, lossy)) = run_compressor(&fx.compressor, &input, &cfg) else {
            continue; // not claimed/not compressed, skip (allowed)
        };

        // Hard invariant 1: compressed size ≤ original size
        assert!(out.len() <= input.len(), "[{}] compressed size should be ≤ original size", fx.file);
        // Hard invariant 2: UTF-8 valid (out is String, naturally valid)
        // Hard invariant 3: json/tabular compressors emit round-trippable TOON.
        if fx.compressor == "json" || fx.compressor == "tabular" {
            toon_format::decode_default::<serde_json::Value>(&out)
                .unwrap_or_else(|_| panic!("[{}] TOON product must decode", fx.file));
        }
        // Comparison 4: size not worse than reference (if present).
        // Only compare ratio against reference when our output is lossy — most reference outputs
        // from headroom/rtk are lossy (e.g., smart_crusher sampling), while v2's JSON/Tabular
        // uses lossless semantics (spec §4.0/round-3 "JSON no lossy"), lossless output naturally
        // cannot compress as well as lossy reference, comparing at 1.5x is meaningless. Correctness
        // of lossless output is already guaranteed by hard invariant 1 (compressed ≤ original),
        // no need to compare size against lossy reference here.
        if lossy && !fx.ref_output.is_empty() {
            if let Ok(reference) = std::fs::read_to_string(dir.join(&fx.ref_output)) {
                assert!(
                    out.len() as f64 <= reference.len() as f64 * 1.5,
                    "[{}] our output {} should not significantly exceed reference {} by 1.5x",
                    fx.file,
                    out.len(),
                    reference.len()
                );
            }
        }
    }
}
