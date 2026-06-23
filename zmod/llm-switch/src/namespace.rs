//! Check if the request contains tools that cannot be expressed by llm-switch v1 (spec §4.1 pre-check).

use codex_protocol::models::ResponseItem;

/// Check if the request contains tools that cannot be expressed by v1 connector (spec §4.1).
/// Covers two hard failure sources: non-function tool definitions in tools, namespaced FunctionCall in input.
pub fn request_has_namespace_tools(req: &codex_api::ResponsesApiRequest) -> bool {
    // ① Tool definitions: any non-"function" type (including missing type) → unexpressible
    let bad_tool_def = req.tools.iter().any(|t| {
        t.get("type").and_then(|v| v.as_str()) != Some("function")
    });
    if bad_tool_def {
        return true;
    }
    // ② Function calls: any FunctionCall with namespace → unexpressible
    req.input.iter().any(|item| {
        matches!(item, ResponseItem::FunctionCall { namespace: Some(_), .. })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_request;

    #[test]
    fn plain_function_tool_is_expressible() {
        let mut req = sample_request();
        req.tools = vec![serde_json::json!({"type": "function", "name": "shell"})];
        assert!(!request_has_namespace_tools(&req));
    }

    #[test]
    fn non_function_tool_definition_triggers() {
        let mut req = sample_request();
        req.tools = vec![serde_json::json!({"type": "web_search"})];
        assert!(request_has_namespace_tools(&req));
    }

    #[test]
    fn tool_without_type_triggers() {
        let mut req = sample_request();
        req.tools = vec![serde_json::json!({"name": "weird"})];
        assert!(request_has_namespace_tools(&req));
    }

    #[test]
    fn namespaced_function_call_in_input_triggers() {
        let mut req = sample_request();
        req.input = vec![codex_protocol::models::ResponseItem::FunctionCall {
            id: None,
            name: "send".into(),
            namespace: Some("mcp__gmail".into()),
            arguments: "{}".into(),
            call_id: "c1".into(),
            internal_chat_message_metadata_passthrough: None,
        }];
        assert!(request_has_namespace_tools(&req));
    }

    #[test]
    fn plain_function_call_in_input_is_expressible() {
        let mut req = sample_request();
        req.input = vec![codex_protocol::models::ResponseItem::FunctionCall {
            id: None,
            name: "shell".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c1".into(),
            internal_chat_message_metadata_passthrough: None,
        }];
        assert!(!request_has_namespace_tools(&req));
    }

    #[test]
    fn empty_request_is_expressible() {
        let req = sample_request();
        assert!(!request_has_namespace_tools(&req));
    }
}
