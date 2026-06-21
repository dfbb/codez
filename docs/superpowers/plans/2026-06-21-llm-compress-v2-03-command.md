# Task 03 — command.rs:call_id → CommandHint 索引

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §4.3。依赖 Task 01(CommandHint 类型已在 01 建立,本任务实现 `index()` 真逻辑)。可与 02/04 并行。

**Goal:** 实现 `command::index`——遍历 `request.input` 的 `FunctionCall`,从 `shell_command.command` / `exec_command.cmd`(单字符串命令行)轻量解析出 `program` + `argv`,建 `call_id → CommandHint` 映射。`CommandHint` 类型与 is_* 方法已由 Task 01 建立,本任务替换 Task 01 的空占位 `index()`。

## Files
- Modify: `zmod/llm-compress/src/command.rs`(替换 Task 01 的占位 `index()`,加 shell 解析)
- Test: `zmod/llm-compress/tests/command_test.rs`

**Interfaces:**
- Consumes: Task 01 的 `CommandHint { program, argv }` 及其 is_* 方法。codex 类型 `codex_protocol::models::ResponseItem::FunctionCall { name, arguments, call_id, .. }`(`codex-rs/protocol/src/models.rs:973`)。
- Produces: `pub fn index(request: &codex_api::ResponsesApiRequest) -> HashMap<String, CommandHint>`(替换占位实现)。

---

- [ ] **Step 1: 写失败测试**

创建 `zmod/llm-compress/tests/command_test.rs`:

```rust
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
    assert!(idx.get("c4").is_none());
}

#[test]
fn non_shell_tool_uses_name_as_program() {
    let r = req(vec![fcall("c5", "my_custom_tool", r#"{"x":1}"#)]);
    let idx = index(&r);
    let hint = idx.get("c5").unwrap();
    assert_eq!(hint.program, "my_custom_tool");
    assert!(hint.argv.is_empty());
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test command_test 2>&1 | head`
Expected: FAIL(占位 index 返回空 HashMap,所有 get 断言失败)

- [ ] **Step 3: 实现真 index() + shell 解析**

把 `zmod/llm-compress/src/command.rs` 的占位 `index()` 替换为(保留文件顶部 CommandHint 定义与 is_* 方法不动,只换 index 函数,并加 use serde_json):

```rust
use codex_protocol::models::ResponseItem;

/// 遍历 request.input 的 FunctionCall,建 call_id → CommandHint 索引。
/// shell_command.command / exec_command.cmd 是单字符串命令行(spec §4.3,已核实)。
/// 解析失败/非 JSON/取不到命令字段 → 该 call_id 不入索引(fail-open)。
pub fn index(request: &codex_api::ResponsesApiRequest) -> HashMap<String, CommandHint> {
    let mut map = HashMap::new();
    for item in &request.input {
        if let ResponseItem::FunctionCall { name, arguments, call_id, .. } = item {
            if let Some(hint) = parse_hint(name, arguments) {
                map.insert(call_id.clone(), hint);
            }
        }
    }
    map
}

fn parse_hint(name: &str, arguments: &str) -> Option<CommandHint> {
    // shell 类工具:从 JSON 取命令行字符串
    let cmdline: Option<String> = match name {
        "shell_command" => serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from)),
        "exec_command" => serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(String::from)),
        _ => None,
    };
    match cmdline {
        Some(line) => {
            let tokens = shell_split(&line);
            let mut it = tokens.into_iter();
            let program = it.next()?;
            let argv: Vec<String> = it.collect();
            Some(CommandHint { program, argv })
        }
        None => {
            // 非 shell 工具:program = name,argv 空。仅当 name 非空时入索引。
            if name.is_empty() {
                None
            } else {
                Some(CommandHint { program: name.to_string(), argv: Vec::new() })
            }
        }
    }
}

/// 轻量 shell 分词:按空白切,尊重单/双引号(只读不执行,失败容忍)。
fn shell_split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    for ch in line.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    has_token = true;
                } else if ch.is_whitespace() {
                    if has_token {
                        out.push(std::mem::take(&mut cur));
                        has_token = false;
                    }
                } else {
                    cur.push(ch);
                    has_token = true;
                }
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}
```

> 注意:文件顶部已有的 `use std::collections::HashMap;`(Task 01)保留;新增 `use codex_protocol::models::ResponseItem;`。删除 Task 01 占位 index 的 `_request` 版本。

- [ ] **Step 4: 运行测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test command_test`
Expected: PASS(5 个)

- [ ] **Step 5: clippy + 提交**

Run: `cd codex-rs && cargo clippy -p codez-llm-compress --all-targets`
Expected: 无 warning

```bash
git add zmod/llm-compress/src/command.rs zmod/llm-compress/tests/command_test.rs
git commit -m "feat(llm-compress-v2): Task03 command.rs call_id→CommandHint 索引 + shell 解析"
```
