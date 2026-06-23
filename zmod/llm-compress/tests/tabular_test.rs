use codez_llm_compress::compress::tabular::TabularCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

/// A padded Markdown table shrinks under TOON.
#[test]
fn padded_markdown_shrinks_to_toon() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "\
| id    | name       | status   |
| ----- | ---------- | -------- |
| 1     | alice      | active   |
| 2     | bob        | inactive |
| 3     | carol      | active   |
| 4     | dave       | active   |
| 5     | erin       | inactive |";

    assert!(c.detect(text, &budget(&cfg)), "padded markdown should be claimed");
    let CompressOutcome::Compressed { text: new, lossy, kind, saved_bytes } =
        c.compress(text, &budget(&cfg))
    else {
        panic!("expected Compressed");
    };
    assert!(!lossy);
    assert_eq!(kind, ContentKind::Toon);
    assert!(new.len() < text.len(), "{} vs {}", new.len(), text.len());
    assert_eq!(saved_bytes, text.len() - new.len());
    // TOON round-trips to an array of {id,name,status} objects.
    let back: serde_json::Value = toon_format::decode_default(&new).unwrap();
    assert_eq!(back[0]["id"], serde_json::json!("1"));
    assert_eq!(back[0]["name"], serde_json::json!("alice"));
    assert_eq!(back[4]["status"], serde_json::json!("inactive"));
}

/// Compact CSV whose TOON form is not smaller → yield to Truncate.
#[test]
fn compact_csv_yields_to_truncate() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob\n3,carol";
    assert!(!c.detect(text, &budget(&cfg)), "no benefit → yield");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

/// Duplicate column names → not a valid table → not claimed.
#[test]
fn duplicate_column_names_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "id,id\n1,2\n3,4";
    assert!(!c.detect(text, &budget(&cfg)));
}

/// Ragged columns → not claimed.
#[test]
fn ragged_columns_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "a,b,c\n1,2\n3,4,5";
    assert!(!c.detect(text, &budget(&cfg)));
}

/// Quoted cells → not claimed (parser can't safely round-trip).
#[test]
fn quoted_cell_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = TabularCompressor;
    let text = "a,b\n\"x,y\",z\n1,2";
    assert!(!c.detect(text, &budget(&cfg)));
}

/// use_toon=false → not claimed.
#[test]
fn use_toon_false_detect_false() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    cfg.json.use_toon = false;
    let c = TabularCompressor;
    let text = "\
| id    | name       | status   |
| ----- | ---------- | -------- |
| 1     | alice      | active   |
| 2     | bob        | inactive |
| 3     | carol      | active   |";
    assert!(!c.detect(text, &budget(&cfg)));
}
