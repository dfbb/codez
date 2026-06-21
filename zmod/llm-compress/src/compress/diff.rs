//! DiffCompressor:识别 unified diff,保留全部变更行与结构头,
//! 仅折叠 hunk 内多余的上下文行。
//!
//! 折叠规则(spec §6):
//! - 变更行(`+`/`-` 开头,但非文件头 `+++`/`---`)、hunk 头(`@@`)、
//!   文件头(`diff --git`/`index`/`--- `/`+++ `)全部保留。
//! - hunk 内上下文行(以单空格开头)仅保留紧邻变更行前后各 `context_lines` 行,
//!   中间折叠为一行裸占位 `[llm-compress: 略 N 行上下文]`。

use crate::router::{Budget, CompressOutcome, Compressor};

/// unified diff 压缩器。
pub struct DiffCompressor;

impl DiffCompressor {
    /// 判断一行是否为 hunk 头 `@@ -a,b +c,d @@`(b、d 可省略)。
    ///
    /// 不引入正则依赖,手写解析等价于 `^@@ -\d+(,\d+)? \+\d+(,\d+)? @@`。
    fn is_hunk_header(line: &str) -> bool {
        // 必须以 "@@ -" 起始。
        let rest = match line.strip_prefix("@@ -") {
            Some(r) => r,
            None => return false,
        };
        // 解析 "\d+(,\d+)? " —— 旧区间。
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // 紧跟一个空格。
        let rest = match rest.strip_prefix(' ') {
            Some(r) => r,
            None => return false,
        };
        // 紧跟 "+"。
        let rest = match rest.strip_prefix('+') {
            Some(r) => r,
            None => return false,
        };
        // 解析 "\d+(,\d+)?" —— 新区间。
        let rest = match Self::consume_range(rest) {
            Some(r) => r,
            None => return false,
        };
        // 紧跟 " @@"。
        rest.starts_with(" @@")
    }

    /// 消费一个 `\d+(,\d+)?` 形式的区间,返回剩余切片;失败返回 None。
    fn consume_range(s: &str) -> Option<&str> {
        // 至少一位数字。
        let first_len = s.bytes().take_while(|b| b.is_ascii_digit()).count();
        if first_len == 0 {
            return None;
        }
        let s = &s[first_len..];
        // 可选 ",\d+"。
        if let Some(after_comma) = s.strip_prefix(',') {
            let len = after_comma.bytes().take_while(|b| b.is_ascii_digit()).count();
            if len == 0 {
                return None;
            }
            Some(&after_comma[len..])
        } else {
            Some(s)
        }
    }

    /// 是否为变更行(`+`/`-` 开头,但排除文件头 `+++ `/`--- `)。
    fn is_change_line(line: &str) -> bool {
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            return false;
        }
        line.starts_with('+') || line.starts_with('-')
    }

    /// 是否为上下文行(hunk 内以单空格开头的未变更行)。
    fn is_context_line(line: &str) -> bool {
        line.starts_with(' ')
    }
}

impl Compressor for DiffCompressor {
    fn name(&self) -> &'static str {
        "diff"
    }

    fn detect(&self, text: &str) -> bool {
        let mut has_minus_header = false;
        let mut has_plus_header = false;
        for line in text.lines() {
            if Self::is_hunk_header(line) {
                return true;
            }
            if line.starts_with("diff --git ") {
                return true;
            }
            if line.starts_with("--- ") {
                has_minus_header = true;
            }
            if line.starts_with("+++ ") {
                has_plus_header = true;
            }
        }
        has_minus_header && has_plus_header
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let ctx = budget.cfg.diff.context_lines;

        // 第一步:把所有行收集为 Vec,便于做"前后窗口"判定。
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.len();

        // 标记每一行是否为上下文行。
        let is_ctx: Vec<bool> = lines.iter().map(|l| Self::is_context_line(l)).collect();
        // 标记每一行是否为"锚点"(变更行 / hunk 头 / 文件头) —— 上下文需围绕变更行保留。
        // 折叠窗口的依据是"与最近变更行的距离",因此先标记变更行位置。
        let is_change: Vec<bool> = lines.iter().map(|l| Self::is_change_line(l)).collect();

        // 计算每一上下文行到"最近变更行"的距离(只在同一连续上下文段内有意义,
        // 但用全局最近变更行距离即可正确实现"紧邻变更行前后各 ctx 行"的语义:
        // 上下文行若其上方 ctx 行内或下方 ctx 行内存在变更行,则保留)。
        let mut keep: Vec<bool> = vec![true; n];

        for i in 0..n {
            if !is_ctx[i] {
                // 非上下文行(变更行 / hunk 头 / 文件头 / 其它)一律保留。
                continue;
            }
            // 上下文行:检查上方 ctx 行内是否有变更行。
            // 向上看 ctx 行 [lo, i),向下看 ctx 行 [i+1, hi)。
            let lo = i.saturating_sub(ctx);
            let hi = (i + ctx + 1).min(n);
            let near_change =
                is_change[lo..i].iter().any(|&c| c) || is_change[i + 1..hi].iter().any(|&c| c);
            keep[i] = near_change;
        }

        // 第二步:按顺序输出,遇到连续被丢弃的上下文段折叠为一行占位。
        let mut out_lines: Vec<String> = Vec::with_capacity(n);
        let mut i = 0;
        let mut any_folded = false;
        while i < n {
            if keep[i] {
                out_lines.push(lines[i].to_string());
                i += 1;
            } else {
                // 收集一段连续的被丢弃上下文行。
                let start = i;
                while i < n && !keep[i] {
                    i += 1;
                }
                let folded = i - start;
                out_lines.push(format!("[llm-compress: 略 {folded} 行上下文]"));
                any_folded = true;
            }
        }

        if !any_folded {
            return CompressOutcome::Unchanged;
        }

        // 重建文本:保留原文末尾换行习惯。原文以 '\n' 结尾则补一个。
        let mut result = out_lines.join("\n");
        if text.ends_with('\n') {
            result.push('\n');
        }

        // 若折叠后体积未减小(占位反而更长),视为 Unchanged。
        if result.len() >= text.len() {
            return CompressOutcome::Unchanged;
        }

        let saved_bytes = text.len() - result.len();
        CompressOutcome::Compressed {
            text: result,
            saved_bytes,
        }
    }
}
