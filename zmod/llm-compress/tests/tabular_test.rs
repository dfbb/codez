use codez_llm_compress::compress::tabular::TabularCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

/// 充分 padding 的 Markdown 表格:schema-form 比原文小。
/// 输入 237 字节,schema-form 输出 159 字节,节省 78 字节。
#[test]
fn padded_markdown_genuinely_shrinks() {
    let cfg = Config::disabled(); // tabular.enabled = true
    let c = TabularCompressor;
    let text = "\
| id    | name       | status   |
| ----- | ---------- | -------- |
| 1     | alice      | active   |
| 2     | bob        | inactive |
| 3     | carol      | active   |
| 4     | dave       | active   |
| 5     | erin       | inactive |";

    assert!(c.detect(text, &budget(&cfg)), "充分 padding 的 markdown 应 detect=true");

    match c.compress(text, &budget(&cfg)) {
        CompressOutcome::Compressed { text: new, lossy, kind, saved_bytes } => {
            assert!(!lossy, "格式重构不删内容");
            assert_eq!(kind, ContentKind::Json);
            assert!(new.len() < text.len(), "schema-form 必须真的更小: {} vs {}", new.len(), text.len());
            assert_eq!(saved_bytes, text.len() - new.len(), "saved_bytes 应如实上报");
            let v: serde_json::Value = serde_json::from_str(&new).expect("valid json");
            assert_eq!(v["_schema"], serde_json::json!(["id", "name", "status"]));
            assert_eq!(v["_rows"][0], serde_json::json!(["1", "alice", "active"]));
            assert_eq!(v["_rows"][4], serde_json::json!(["5", "erin", "inactive"]));
        }
        _ => panic!("expect Compressed"),
    }
}

/// 紧凑 CSV:schema-form 比原文大,应让位给 Truncate。
#[test]
fn compact_csv_yields_to_truncate() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob\n3,carol";
    assert!(!c.detect(text, &budget(&cfg)), "紧凑 CSV schema-form 更大,应 detect=false 让给 Truncate");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

/// 小 Markdown 表格(2 行),schema-form 比原文大,应让位。
#[test]
fn small_markdown_yields() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "| id | name |\n|----|------|\n| 1 | alice |\n| 2 | bob |";
    assert!(!c.detect(text, &budget(&cfg)), "小 markdown schema-form 更大,应 detect=false");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

/// 重复列名 → detect false。
#[test]
fn duplicate_column_names_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "id,id\n1,2\n3,4";
    assert!(!c.detect(text, &budget(&cfg)), "重复列名 → detect false 让 Truncate");
}

/// 列数不齐 → detect false。
#[test]
fn ragged_columns_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b,c\n1,2\n3,4,5";
    assert!(!c.detect(text, &budget(&cfg)), "列数不齐 → detect false");
}

/// 含引号单元格 → detect false。
#[test]
fn quoted_cell_detect_false() {
    let cfg = Config::disabled();
    let c = TabularCompressor;
    let text = "a,b\n\"x,y\",z\n1,2";
    assert!(!c.detect(text, &budget(&cfg)), "含引号转义单元格 → detect false");
}

/// tabular.enabled=false → detect false。
#[test]
fn disabled_config_detect_false() {
    let mut cfg = Config::disabled();
    cfg.tabular.enabled = false;
    let c = TabularCompressor;
    let text = "id,name\n1,alice\n2,bob";
    assert!(!c.detect(text, &budget(&cfg)));
}
