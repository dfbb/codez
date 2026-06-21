use codez_llm_compress::compress::tabular::TabularCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn csv_with_header_to_schema() {
    let cfg = Config::disabled(); // tabular.enabled 默认 true
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob\n3,carol";
    assert!(c.detect(text, &budget(&cfg)));
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &budget(&cfg)) {
        assert!(!lossy, "格式重构不删内容");
        assert_eq!(kind, ContentKind::Json);
        let v: serde_json::Value = serde_json::from_str(&new).expect("valid json");
        assert_eq!(v["_schema"], serde_json::json!(["id","name"]));
        assert_eq!(v["_rows"][0], serde_json::json!(["1","alice"]));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn markdown_table_to_schema() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "| id | name |\n|----|------|\n| 1 | alice |\n| 2 | bob |";
    assert!(c.detect(text, &budget(&cfg)));
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &budget(&cfg)) {
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["_schema"], serde_json::json!(["id","name"]));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn duplicate_column_names_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "id,id\n1,2\n3,4";
    assert!(!c.detect(text, &budget(&cfg)), "重复列名 → detect false 让 Truncate");
}

#[test]
fn ragged_columns_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b,c\n1,2\n3,4,5";
    assert!(!c.detect(text, &budget(&cfg)), "列数不齐 → detect false");
}

#[test]
fn quoted_cell_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b\n\"x,y\",z\n1,2";
    assert!(!c.detect(text, &budget(&cfg)), "含引号转义单元格 → detect false");
}

#[test]
fn disabled_config_detect_false() {
    let mut cfg = Config::disabled();
    cfg.tabular.enabled = false;
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob";
    assert!(!c.detect(text, &budget(&cfg)));
}
