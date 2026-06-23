//! 请求是否含 llm-switch v1 不可表达的工具(spec §4.1 预检)。

use codex_protocol::models::ResponseItem;

/// 请求是否含 v1 连接器不可表达的工具(spec §4.1)。
/// 覆盖两个硬失败来源:tools 里的非 function 工具定义、input 里的 namespaced FunctionCall。
pub fn request_has_namespace_tools(req: &codex_api::ResponsesApiRequest) -> bool {
    // ① 工具定义:任一非 "function" 类型(含缺失 type)→ 不可表达
    let bad_tool_def = req.tools.iter().any(|t| {
        t.get("type").and_then(|v| v.as_str()) != Some("function")
    });
    if bad_tool_def {
        return true;
    }
    // ② 函数调用:任一带 namespace 的 FunctionCall → 不可表达
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
