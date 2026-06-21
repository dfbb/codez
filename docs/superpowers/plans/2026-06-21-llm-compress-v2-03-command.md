# Task 03 — command.rs: call_id → CommandHint index

> Part of `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §4.3. Depends on Task 01 (the `CommandHint` type is established in 01; this task implements the real `index()` logic). Can run in parallel with 02/04.

**Goal:** Implement `command::index` — iterate over the `FunctionCall`s in `request.input`, lightly parse a `program` + `argv` out of `shell_command.command` / `exec_command.cmd` (a single command-line string), and build a `call_id → CommandHint` map. The `CommandHint` type and its is_* methods were established in Task 01; this task replaces Task 01's empty placeholder `index()`.

## Files
- Modify: `zmod/llm-compress/src/command.rs` (replace Task 01's placeholder `index()`, add shell parsing)
- Test: `zmod/llm-compress/tests/command_test.rs`

**Interfaces:**
- Consumes: Task 01's `CommandHint { program, argv }` and its is_* methods. The codex type `codex_protocol::models::ResponseItem::FunctionCall { name, arguments, call_id, .. }` (`codex-rs/protocol/src/models.rs:973`).
- Produces: `pub fn index(request: &codex_api::ResponsesApiRequest) -> HashMap<String, CommandHint>` (replaces the placeholder implementation).

---

- [ ] **Step 1: Write a failing test**

Create `zmod/llm-compress/tests/command_test.rs`:

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

- [ ] **Step 2: Run and confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test command_test 2>&1 | head`
Expected: FAIL (the placeholder index returns an empty HashMap, so every get assertion fails)

- [ ] **Step 3: Implement the real index() + shell parsing**

Replace the placeholder `index()` in `zmod/llm-compress/src/command.rs` with the following (keep the CommandHint definition and is_* methods at the top of the file unchanged, only swap out the index function, and add use serde_json):

```rust
use codex_protocol::models::ResponseItem;

/// Iterate over the FunctionCalls in request.input, building a call_id → CommandHint index.
/// shell_command.command / exec_command.cmd is a single command-line string (spec §4.3, verified).
/// Parse failure / non-JSON / missing command field → that call_id is not indexed (fail-open).
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
    // shell-style tools: extract the command-line string from JSON
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
            // non-shell tool: program = name, argv empty. Indexed only when name is non-empty.
            if name.is_empty() {
                None
            } else {
                Some(CommandHint { program: name.to_string(), argv: Vec::new() })
            }
        }
    }
}

/// Lightweight shell tokenization: split on whitespace, honoring single/double quotes (read-only, never executed, fault-tolerant).
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

> Note: the existing `use std::collections::HashMap;` at the top of the file (Task 01) is kept; add `use codex_protocol::models::ResponseItem;`. Delete the `_request` version of Task 01's placeholder index.

- [ ] **Step 4: Run the tests and pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test command_test`
Expected: PASS (5 tests)

- [ ] **Step 5: clippy + commit**

Run: `cd codex-rs && cargo clippy -p codez-llm-compress --all-targets`
Expected: no warnings

```bash
git add zmod/llm-compress/src/command.rs zmod/llm-compress/tests/command_test.rs
git commit -m "feat(llm-compress-v2): Task03 command.rs call_id→CommandHint index + shell parsing"
```
