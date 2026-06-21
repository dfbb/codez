//! call_id → 命令提示。类型与判别在此(Task 01 地基);index() 解析在 Task 03。
use std::collections::HashMap;
use codex_protocol::models::ResponseItem;

/// 从 FunctionCall 解析出的命令提示,仅作路由提示用(spec §4.3)。
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
    pub fn is_ls_like(&self) -> bool {
        matches!(self.program.rsplit('/').next(), Some("ls") | Some("tree") | Some("find"))
    }
    pub fn is_test_runner(&self) -> bool {
        matches!(
            self.program.rsplit('/').next(),
            Some("pytest") | Some("jest") | Some("vitest") | Some("cargo") | Some("go") | Some("rspec")
        )
    }
}

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
    // shell 类工具:从 JSON 取命令行字符串;解析失败 → 不入索引
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
