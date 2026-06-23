//! call_id → command hint. Type and discriminant here (Task 01 foundation); index() parsing in Task 03.
use std::collections::HashMap;
use codex_protocol::models::ResponseItem;

/// Command hint parsed from FunctionCall, used only as a routing hint (spec §4.3).
#[derive(Debug, Clone, Default)]
pub struct CommandHint {
    pub program: String,
    pub argv: Vec<String>,
}

impl CommandHint {
    pub fn is_git_diff(&self) -> bool {
        self.program.ends_with("git") && matches!(self.argv.first().map(String::as_str), Some("diff") | Some("show"))
    }
    pub fn is_grep(&self) -> bool {
        matches!(self.program.rsplit('/').next(), Some("grep") | Some("rg") | Some("ripgrep"))
    }
    pub fn is_test_runner(&self) -> bool {
        matches!(
            self.program.rsplit('/').next(),
            Some("pytest") | Some("jest") | Some("vitest") | Some("cargo") | Some("go") | Some("rspec")
        )
    }
}

/// Iterate through request.input FunctionCalls and build a call_id → CommandHint index.
/// shell_command.command / exec_command.cmd are single-string command lines (spec §4.3, verified).
/// Parse failure / non-JSON / unable to fetch command field → that call_id not indexed (fail-open).
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
    // Shell-like tools: fetch command line string from JSON; parse failure → not indexed
    match name {
        "shell_command" => {
            let line = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from))?;
            let tokens = shell_split(&line);
            let mut it = tokens.into_iter();
            let program = it.next()?;
            Some(CommandHint { program, argv: it.collect() })
        }
        "exec_command" => {
            let line = serde_json::from_str::<serde_json::Value>(arguments)
                .ok()
                .and_then(|v| v.get("cmd").and_then(|c| c.as_str()).map(String::from))?;
            let tokens = shell_split(&line);
            let mut it = tokens.into_iter();
            let program = it.next()?;
            Some(CommandHint { program, argv: it.collect() })
        }
        _ => {
            // Non-shell tools: program = name, argv empty. Only indexed if name is non-empty.
            if name.is_empty() {
                None
            } else {
                Some(CommandHint { program: name.to_string(), argv: Vec::new() })
            }
        }
    }
}

/// Lightweight shell tokenization: split by whitespace, respect single/double quotes (read-only, no execution, failure tolerant).
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
