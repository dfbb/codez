use codez_llm_compress::command::index;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::ResponseItem;

fn req(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "m".to_string(),
        instructions: String::new(),
        input,
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn fcall(call_id: &str, name: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
        metadata: None,
    }
}

#[test]
fn parses_shell_command_string() {
    let r = req(vec![fcall("c1", "shell_command", r#"{"command":"git diff HEAD~1"}"#)]);
    let idx = index(&r);
    let hint = idx.get("c1").expect("c1 indexed");
    assert_eq!(hint.program, "git");
    assert_eq!(hint.argv, vec!["diff".to_string(), "HEAD~1".to_string()]);
    assert!(hint.is_git_diff());
}

#[test]
fn parses_exec_command_cmd_field() {
    let r = req(vec![fcall("c2", "exec_command", r#"{"cmd":"rg --json pattern src/"}"#)]);
    let idx = index(&r);
    let hint = idx.get("c2").expect("c2 indexed");
    assert_eq!(hint.program, "rg");
    assert!(hint.is_grep());
}

#[test]
fn handles_quoted_args() {
    let r = req(vec![fcall("c3", "shell_command", r#"{"command":"grep \"foo bar\" file.txt"}"#)]);
    let idx = index(&r);
    let hint = idx.get("c3").unwrap();
    assert_eq!(hint.program, "grep");
    assert_eq!(hint.argv, vec!["foo bar".to_string(), "file.txt".to_string()]);
}

#[test]
fn non_json_arguments_skipped() {
    let r = req(vec![fcall("c4", "shell_command", "not json {")]);
    let idx = index(&r);
    assert!(!idx.contains_key("c4"));
}

#[test]
fn non_shell_tool_uses_name_as_program() {
    let r = req(vec![fcall("c5", "my_custom_tool", r#"{"x":1}"#)]);
    let idx = index(&r);
    let hint = idx.get("c5").unwrap();
    assert_eq!(hint.program, "my_custom_tool");
    assert!(hint.argv.is_empty());
}
