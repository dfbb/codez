use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
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
    Budget { cfg, cmd: None }
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
fn rle_folds_run_but_skips_existing_marker_objects() {
    // 输入: 6 个相同对象({"a":1}) + 已存在的 marker({"_llm_dup_prev":3}) + 再一个 {"a":1}
    // 预期:
    //   (a) 6 个相同对象折叠为 [{"a":1}, {"_llm_dup_prev":5}](extra=5)
    //   (b) 已有 marker {"_llm_dup_prev":3} 原样保留(value=3,未被改写)
    //   (c) marker 后的最后一个 {"a":1} 不跨越 marker 与前面合并
    // 输入 77 字节,折叠后 57 字节,saved=20 → 必然 Compressed
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    let text =
        r#"[{"a":1},{"a":1},{"a":1},{"a":1},{"a":1},{"a":1},{"_llm_dup_prev":3},{"a":1}]"#;
    let b = budget_t05(&cfg);
    let CompressOutcome::Compressed { text: new, lossy, .. } = c.compress(text, &b) else {
        panic!("expected Compressed: 6 连续重复应触发 RLE 折叠以节省字节");
    };
    assert!(!lossy, "RLE 不删内容,lossy 必须为 false");

    let v: serde_json::Value = serde_json::from_str(&new).expect("压缩产物必须是合法 JSON");
    let arr = v.as_array().expect("压缩产物必须是数组");

    // (a) 前两项:首项 {"a":1} + 折叠计数对象 {"_llm_dup_prev":5}
    assert_eq!(arr[0], serde_json::json!({"a": 1}), "首项必须是 {{\"a\":1}}");
    assert_eq!(
        arr[1],
        serde_json::json!({"_llm_dup_prev": 5}),
        "折叠 6 个重复应产生 extra=5 的 marker"
    );

    // (b) 已有 marker 原样保留:value 仍为 3(非 5、非其他)
    assert_eq!(
        arr[2],
        serde_json::json!({"_llm_dup_prev": 3}),
        "原输入中的 marker(value=3)必须原样保留,不得被改写"
    );

    // (c) marker 之后的孤立 {"a":1} 未被合并进前面的折叠
    assert_eq!(arr[3], serde_json::json!({"a": 1}), "marker 后的 {{\"a\":1}} 应独立保留");

    // 结果恰好是这 4 项
    assert_eq!(arr.len(), 4, "输出数组长度应为 4");
}
