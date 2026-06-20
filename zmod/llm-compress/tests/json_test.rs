use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

/// 构造一个启用且 json 阈值偏小的配置,便于触发压缩。
fn cfg_small() -> Config {
    let mut c = Config::disabled();
    c.json.max_array_items = 6; // ceil=3 头 + floor=3 尾
    c.json.max_depth = 2;
    c
}

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg }
}

#[test]
fn detect_accepts_valid_json_rejects_garbage() {
    let c = JsonCompressor;
    assert!(c.detect(r#"{"a":1,"b":[1,2,3]}"#));
    assert!(c.detect("[1, 2, 3]"));
    assert!(c.detect("\"a quoted string is valid json\""));
    assert!(c.detect("123"));
    // 非 JSON
    assert!(!c.detect("not json {"));
    assert!(!c.detect("{unquoted: key}"));
    assert!(!c.detect(""));
}

#[test]
fn long_array_is_sampled_with_placeholder_element() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // 20 个元素,远超 max_array_items=6
    let arr: Vec<i64> = (0..20).collect();
    let text = serde_json::to_string(&arr).unwrap();

    let out = c.compress(&text, &budget(&cfg));
    let new = match out {
        CompressOutcome::Compressed { text, saved_bytes } => {
            assert!(saved_bytes > 0);
            text
        }
        CompressOutcome::Unchanged => panic!("expected compression for a 20-element array"),
    };

    // 产物必须是合法 JSON
    let v: Value = serde_json::from_str(&new).expect("compressed output must be valid JSON");
    let items = v.as_array().expect("top-level array");
    // 6 个保留(3 头 + 3 尾) + 1 个占位 = 7
    assert_eq!(items.len(), 7);
    // 头 3:0,1,2;尾 3:17,18,19
    assert_eq!(items[0], serde_json::json!(0));
    assert_eq!(items[2], serde_json::json!(2));
    assert_eq!(items[4], serde_json::json!(17));
    assert_eq!(items[6], serde_json::json!(19));
    // 中间是占位字符串 "…(N more)",N = 20 - 6 = 14
    assert_eq!(items[3], serde_json::json!("…(14 more)"));
}

#[test]
fn output_is_always_valid_json() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // 混合:长数组 + 嵌套对象 + 超深
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

#[test]
fn deep_subtree_replaced_by_ellipsis_string() {
    // max_depth=2:depth 0 顶层对象,depth 1 值,depth 2 值,depth 3 起的容器被替换。
    let cfg = cfg_small();
    let c = JsonCompressor;
    let text = r#"{"a":{"b":{"c":{"d":{"e":1}}}}}"#;
    let out = c.compress(text, &budget(&cfg));
    let new = match out {
        CompressOutcome::Compressed { text, .. } => text,
        CompressOutcome::Unchanged => panic!("expected deep nesting to be trimmed"),
    };
    let v: Value = serde_json::from_str(&new).expect("valid JSON");
    // 顺着 a/b 走到被替换处,应在某层遇到字符串 "…"
    let mut found_ellipsis = false;
    let mut node = &v;
    for key in ["a", "b", "c", "d", "e"] {
        if node == &serde_json::json!("…") {
            found_ellipsis = true;
            break;
        }
        match node.get(key) {
            Some(child) => node = child,
            None => break,
        }
    }
    if node == &serde_json::json!("…") {
        found_ellipsis = true;
    }
    assert!(found_ellipsis, "expected a \"…\" placeholder for the over-deep subtree, got: {new}");
}

#[test]
fn small_json_is_unchanged() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    // 数组短(<=6)、嵌套浅(<=2):无可压。
    let text = r#"{"a":[1,2,3],"b":{"c":1}}"#;
    matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged)
        .then_some(())
        .expect("small json should be Unchanged");
}

#[test]
fn saved_bytes_equals_len_delta() {
    let cfg = cfg_small();
    let c = JsonCompressor;
    let arr: Vec<i64> = (0..50).collect();
    let text = serde_json::to_string(&arr).unwrap();
    if let CompressOutcome::Compressed { text: new, saved_bytes } = c.compress(&text, &budget(&cfg)) {
        assert_eq!(saved_bytes, text.len() - new.len());
        assert!(new.len() < text.len());
    } else {
        panic!("expected compression");
    }
}
