use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::Value;

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

#[test]
fn detect_accepts_object_and_array_rejects_scalar_and_garbage() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000; // large enough, no yield to Truncate
    let c = JsonCompressor;
    let b = budget(&cfg);
    // Object/array that shrink under TOON are claimed.
    assert!(c.detect(r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"}]"#, &b));
    // Scalars are never claimed.
    assert!(!c.detect("\"a quoted string\"", &b));
    assert!(!c.detect("123", &b));
    // Non-JSON.
    assert!(!c.detect("not json {", &b));
    assert!(!c.detect("{unquoted: key}", &b));
    assert!(!c.detect("", &b));
}

#[test]
fn compress_emits_round_trippable_toon_for_homogeneous_array() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"},{"id":3,"name":"carol"}]"#;
    let CompressOutcome::Compressed { text: new, lossy, kind, saved_bytes } =
        c.compress(text, &budget(&cfg))
    else {
        panic!("expected Compressed");
    };
    assert!(!lossy, "TOON is lossless");
    assert_eq!(kind, ContentKind::Toon);
    assert_eq!(saved_bytes, text.len() - new.len());
    // Round-trips back to the original value.
    let back: Value = toon_format::decode_default(&new).unwrap();
    assert_eq!(back, serde_json::from_str::<Value>(text).unwrap());
    // Tabular header present (uniform object array).
    assert!(new.contains("{id,name}:"), "got: {new:?}");
}

#[test]
fn detect_yields_to_truncate_when_toon_exceeds_max_bytes() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 20; // tiny
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"},{"id":3,"name":"carol"}]"#;
    assert!(!c.detect(text, &budget(&cfg)), "TOON over max_bytes → yield to Truncate");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

#[test]
fn detect_false_when_toon_not_smaller() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    // A heterogeneous nested array whose TOON form (indented) is not strictly
    // smaller than the input (toon-format 0.5 produces 86 bytes for 77-byte input).
    let text = r#"[{"id":1,"name":"alice","tags":["x","y"]},{"id":2,"name":"bob","tags":["z"]}]"#;
    assert!(!c.detect(text, &budget(&cfg)), "no size benefit → not claimed");
}

#[test]
fn use_toon_false_disables_claim() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    cfg.json.use_toon = false;
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"}]"#;
    assert!(!c.detect(text, &budget(&cfg)), "use_toon=false → detect false");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

#[test]
fn toon_output_is_deterministic_across_runs() {
    // Cache stability: compression must be a pure function of content.
    // Encoding the same input many times must yield byte-identical TOON.
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    // Homogeneous array that compresses well under TOON (71 → 41 bytes).
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"},{"id":3,"name":"carol"}]"#;
    let first = match c.compress(text, &budget(&cfg)) {
        CompressOutcome::Compressed { text, .. } => text,
        CompressOutcome::Unchanged => panic!("expected Compressed"),
    };
    for _ in 0..20 {
        let CompressOutcome::Compressed { text: again, .. } = c.compress(text, &budget(&cfg))
        else {
            panic!("expected Compressed");
        };
        assert_eq!(again, first, "TOON output must be byte-identical across runs");
    }
}
