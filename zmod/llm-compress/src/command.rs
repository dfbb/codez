//! call_id → 命令提示。类型与判别在此(Task 01 地基);index() 解析在 Task 03。
use std::collections::HashMap;

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
        let p = self.program.rsplit('/').next().unwrap_or("");
        matches!(p, "pytest" | "jest" | "vitest" | "cargo" | "go" | "rspec")
    }
}

/// Task 03 实现:遍历 request.input 的 FunctionCall 建 call_id→CommandHint 索引。
/// 本任务先给空实现占位,使 lib 可编译;Task 03 替换。
pub fn index(_request: &codex_api::ResponsesApiRequest) -> HashMap<String, CommandHint> {
    HashMap::new()
}
