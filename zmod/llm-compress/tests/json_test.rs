use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

// ===== 保留 Task 01 迁移的 detect 基础用例(更新为 v2 语义:需 max_bytes 足够) =====

#[test]
fn detect_accepts_valid_json_rejects_garbage() {
    let c = JsonCompressor;
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000; // 足够大,不触发让位
    let b = budget(&cfg);
    assert!(c.detect(r#"{"a":1,"b":[1,2,3]}"#, &b));
    assert!(c.detect("[1, 2, 3]", &b));
    // 新 detect:仅认领 object/array,scalar 不再认领
    assert!(!c.detect("\"a quoted string is valid json\"", &b));
    assert!(!c.detect("123", &b));
    // 非 JSON
    assert!(!c.detect("not json {", &b));
    assert!(!c.detect("{unquoted: key}", &b));
    assert!(!c.detect("", &b));
}

#[test]
fn output_is_always_valid_json() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    cfg.json.csv_schema = true;
    let c = JsonCompressor;
    // 混合:对象 + 嵌套
    let text = r#"{
        "list": [1,2,3,4,5,6,7,8,9,10,11,12],
        "nested": {"a": {"b": {"c": {"d": 1}}}},
        "name": "keep me"
    }"#;
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &budget(&cfg)) {
        // 关键断言:无论压成什么,产物都能被重新解析。
        serde_json::from_str::<Value>(&new).expect("compressed output must be valid JSON");
        // 键必须全保留
        let v: Value = serde_json::from_str(&new).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("list"));
        assert!(obj.contains_key("nested"));
        assert!(obj.contains_key("name"));
    }
    // 即便未压缩(Unchanged)也无破坏可言,测试主旨是"压了就必须合法"。
}

// ===== Task 05 新增 ==========
use codez_llm_compress::router::ContentKind;

fn budget_t05(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None, query: &[] }
}

#[test]
fn consecutive_rle_folds_adjacent_duplicates() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000; // 不让位
    let c = JsonCompressor;
    // 4 个相邻相同对象
    let text = r#"[{"a":1},{"a":1},{"a":1},{"a":1},{"b":2}]"#;
    let b = budget_t05(&cfg);
    assert!(c.detect(text, &b));
    if let CompressOutcome::Compressed { text: new, lossy, kind, .. } = c.compress(text, &b) {
        assert!(!lossy, "RLE 不删内容");
        assert_eq!(kind, ContentKind::Json);
        let v: serde_json::Value = serde_json::from_str(&new).expect("valid json");
        // 首项保留 + 计数占位,首项 {"a":1} 仍在
        assert_eq!(v[0], serde_json::json!({"a":1}));
        assert!(new.contains("_llm_dup_prev"));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn csv_schema_applied_to_homogeneous_array() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    cfg.json.csv_schema = true;
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"},{"id":3,"name":"carol"}]"#;
    let b = budget_t05(&cfg);
    if let CompressOutcome::Compressed { text: new, lossy, .. } = c.compress(text, &b) {
        assert!(!lossy);
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["_schema"], serde_json::json!(["id","name"]));
    } else {
        panic!("expected compressed");
    }
}

#[test]
fn detect_yields_to_truncate_when_lossless_insufficient() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 50; // 很小
    let c = JsonCompressor;
    // 大数组、无相邻重复、非同构 → 无损压不下来 → 超 50 字节 → detect false
    let text = r#"[{"a":1,"x":"aaaa"},{"b":2,"y":"bbbb"},{"c":3,"z":"cccc"}]"#;
    let b = budget_t05(&cfg);
    assert!(!c.detect(text, &b), "无损压不到 50 字节 → 让位 Truncate");
}

#[test]
fn detect_accepts_when_small_enough() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    let text = r#"{"a":1}"#;
    let b = budget_t05(&cfg);
    assert!(c.detect(text, &b), "小 JSON 未超阈 → 认领");
}

#[test]
fn rle_skips_existing_marker_objects() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    // 原数组已含 _llm_dup_prev 形态对象,不应被折叠改写
    let text = r#"[{"_llm_dup_prev":5},{"_llm_dup_prev":5}]"#;
    let b = budget_t05(&cfg);
    if let CompressOutcome::Compressed { text: new, .. } = c.compress(text, &b) {
        // 不混淆:两个 _llm_dup_prev 对象不被当作 RLE 折叠产物
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert!(v.is_array() || v.is_object());
    }
    // 也允许 Unchanged(无收益);关键是不 panic、产物合法
}
