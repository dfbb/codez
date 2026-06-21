//! 兜底文本截断器:剥 ANSI、按行 head/tail 保留、中间裸占位、必要时按 max_bytes
//! 在 UTF-8 字符边界硬截断。detect() 永真,作为 ContentRouter 链尾兜底。
//! 不可逆但保守:小输入直接 Unchanged。

use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};

/// 兜底文本压缩器。无状态,单元结构体。
pub struct TruncateCompressor;

impl Compressor for TruncateCompressor {
    fn name(&self) -> &'static str {
        "truncate"
    }

    /// 永真:认领一切内容(链尾兜底)。
    fn detect(&self, _text: &str, _budget: &Budget) -> bool {
        true
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        let cfg = &budget.cfg.truncate;
        let original_len = text.len();

        // ① 剥 ANSI 转义序列。
        let cleaned = strip_ansi(text);

        // ② 按行切。
        let lines: Vec<&str> = cleaned.split('\n').collect();
        let line_count = lines.len();

        // ③ 行数与字节都未超阈 → 保守不动。
        //    注意:即使剥了 ANSI,只要未超阈也返回 Unchanged(不为"仅去色"而改写,
        //    避免无谓不可逆改动;saved_bytes 口径也要求 new < original 才算压缩)。
        if line_count <= cfg.head_lines + cfg.tail_lines && cleaned.len() <= cfg.max_bytes {
            return CompressOutcome::Unchanged;
        }

        // ④ 保留前 head_lines + 后 tail_lines,中间一行裸占位。
        let mut result = build_head_tail(&lines, cfg.head_lines, cfg.tail_lines);

        // ⑤ 若仍超 max_bytes,按字符边界硬截断 + 追加截断标记。
        if result.len() > cfg.max_bytes {
            result = hard_truncate(&result, cfg.max_bytes);
        }

        // ⑥ 计算 saved_bytes;须确有缩减。
        if result.len() < original_len {
            let saved_bytes = original_len - result.len();
            CompressOutcome::Compressed { text: result, saved_bytes, lossy: true, kind: ContentKind::Text }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// 手写 ANSI(CSI)剥离:跳过 `\x1b[` 起、到终止字节(`@`..=`~`,0x40..=0x7E)止的序列。
/// 覆盖 `\x1b[0m`、`\x1b[1;31m`、`\x1b[2K` 等;对孤立的 `\x1b` 也安全跳过。
/// 非 CSI 的其它 ESC 形式(如 `\x1bP`…)在工具输出里罕见,这里保守地丢弃紧跟的单字节,
/// 不追求覆盖全部 ECMA-48,够用即可(spec §6 仅要求剥常见颜色/控制码)。
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI:跳到终止字节(0x40..=0x7E)为止(含终止字节)。
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                // j 指向终止字节(若存在);整段连终止字节一并丢弃。
                i = if j < bytes.len() { j + 1 } else { j };
            } else {
                // 孤立 ESC 或非 CSI:丢弃 ESC 及其后一个字节(若有)。
                i = if i + 1 < bytes.len() { i + 2 } else { i + 1 };
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    // strip_ansi 只删除完整 ASCII 控制序列(ESC=0x1b 与 0x40..=0x7e 均为单字节 ASCII),
    // 不会切断多字节 UTF-8 字符,故 from_utf8 必然成功;失败时保守回退原文。
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// 拼装"前 head 行 + 占位标记 + 后 tail 行"。
/// 若 head+tail 覆盖了全部行(无中间省略),则不插占位标记,原样拼回。
fn build_head_tail(lines: &[&str], head: usize, tail: usize) -> String {
    let n = lines.len();

    // head+tail 覆盖全部行 → 无省略,直接拼回(可能仍因字节超阈走硬截断)。
    if head + tail >= n {
        return lines.join("\n");
    }

    let head_part = &lines[..head];
    let tail_part = &lines[n - tail..];
    let omitted = &lines[head..n - tail];

    let omitted_lines = omitted.len();
    // 省略字节数:被省略各行的字节 + 行间换行(omitted_lines - 1 个,若 >0)。
    let mut omitted_bytes: usize = omitted.iter().map(|l| l.len()).sum();
    if omitted_lines > 0 {
        omitted_bytes += omitted_lines - 1;
    }

    let marker = format!("[llm-compress: 略 {omitted_lines} 行 / {omitted_bytes} 字节]");

    let mut parts: Vec<String> = Vec::with_capacity(head + 1 + tail);
    for line in head_part {
        parts.push(line.to_string());
    }
    parts.push(marker);
    for line in tail_part {
        parts.push(line.to_string());
    }
    parts.join("\n")
}

/// 在 UTF-8 字符边界把 `text` 截到 `max_bytes` 以内,追加截断标记。
/// 预留标记所需字节,确保最终结果整体 ≤ 一个合理上界(尽量贴近 max_bytes)。
fn hard_truncate(text: &str, max_bytes: usize) -> String {
    const SUFFIX: &str = "\n[llm-compress: 截断至 max_bytes]";

    // 预算:正文允许的字节上限 = max_bytes 减去标记长度(若 max_bytes 比标记还小,则正文取 0)。
    let budget = max_bytes.saturating_sub(SUFFIX.len());

    // 找 ≤ budget 的最大 UTF-8 字符边界。
    let mut cut = 0usize;
    for (idx, ch) in text.char_indices() {
        let end = idx + ch.len_utf8();
        if end <= budget {
            cut = end;
        } else {
            break;
        }
    }

    let mut result = String::with_capacity(cut + SUFFIX.len());
    result.push_str(&text[..cut]);
    result.push_str(SUFFIX);
    result
}
