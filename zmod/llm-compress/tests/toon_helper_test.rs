use codez_llm_compress::compress::toon::encode_checked;
use serde_json::json;

#[test]
fn encodes_homogeneous_array_and_round_trips() {
    let v = json!([{"id":1,"name":"alice"},{"id":2,"name":"bob"}]);
    let toon = encode_checked(&v).expect("homogeneous array encodes + round-trips");
    // TOON tabular header for an array of uniform objects.
    assert!(toon.contains("[2]{id,name}:"), "got: {toon:?}");
    assert!(toon.len() < v.to_string().len());
}

#[test]
fn encodes_nested_object_and_round_trips() {
    let v = json!({"list":[1,2,3],"nested":{"a":{"b":1}},"name":"keep"});
    let toon = encode_checked(&v).expect("nested object encodes + round-trips");
    // Re-decoding must reproduce the original value exactly.
    let back: serde_json::Value = toon_format::decode_default(&toon).unwrap();
    assert_eq!(back, v);
}

#[test]
fn rejects_when_round_trip_loses_information() {
    // Float 1.0 encodes as "1" in TOON and decodes back as integer 1,
    // so the round-trip self-check must FAIL and return None (fall back).
    let v = json!({"x": 1.0});
    assert!(encode_checked(&v).is_none(), "lossy round-trip must be rejected");
}

#[test]
fn preserves_ambiguous_strings() {
    // Strings that look like numbers/bools must survive (TOON quotes them).
    let v = json!({"code":"007","flag":"true"});
    let toon = encode_checked(&v).expect("ambiguous strings round-trip via quoting");
    let back: serde_json::Value = toon_format::decode_default(&toon).unwrap();
    assert_eq!(back, v);
}
