use codez_llm_compress::compress::schema::to_schema_form;
use serde_json::json;

#[test]
fn homogeneous_object_array_to_schema() {
    let v = json!([{"id":1,"name":"a"},{"id":2,"name":"b"}]);
    let out = to_schema_form(&v).expect("homogeneous array → schema");
    assert_eq!(out["_schema"], json!(["id","name"]));
    assert_eq!(out["_rows"], json!([[1,"a"],[2,"b"]]));
}

#[test]
fn heterogeneous_keys_return_none() {
    let v = json!([{"id":1},{"name":"b"}]);
    assert!(to_schema_form(&v).is_none());
}

#[test]
fn non_array_returns_none() {
    assert!(to_schema_form(&json!({"a":1})).is_none());
    assert!(to_schema_form(&json!("str")).is_none());
}

#[test]
fn nested_values_preserved_in_rows() {
    let v = json!([{"id":1,"meta":{"x":9}},{"id":2,"meta":{"x":8}}]);
    let out = to_schema_form(&v).unwrap();
    // 嵌套对象原样放进行,不扁平
    assert_eq!(out["_rows"][0][1], json!({"x":9}));
}

#[test]
fn single_element_returns_none() {
    let v = json!([{"id":1}]);
    assert!(to_schema_form(&v).is_none());
}
