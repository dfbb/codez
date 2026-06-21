//! 错误输出保护门(spec §4.5/C2):错误/异常且 < 阈值 → 整段不压。
//! 优先级最高:在所有预处理之前判定,命中即整段逐字节不变(spec §2/§4.5 #7)。

use crate::command::CommandHint;
use crate::config::Config;

const STRONG_ERROR_MARKERS: &[&str] = &[
    "traceback (most recent call last)",
    "panic",
    "error:",
    "exception",
    "fatal:",
    "segmentation fault",
];

/// 文本含强错误指示符且 len < cfg.protect.error_max_bytes → true(整段不压)。
/// error_max_bytes==0 → 关闭保护,恒 false。
pub fn should_protect(text: &str, cmd: Option<&CommandHint>, cfg: &Config) -> bool {
    let limit = cfg.protect.error_max_bytes;
    if limit == 0 {
        return false;
    }
    if text.len() >= limit {
        return false;
    }
    let lower = text.to_lowercase();
    let has_error = STRONG_ERROR_MARKERS.iter().any(|m| lower.contains(m));
    // 命令提示辅助:test runner 输出含 fail/error 时提高保护倾向。
    let test_failed = cmd.is_some_and(|c| c.is_test_runner())
        && (lower.contains("fail") || lower.contains("error"));
    has_error || test_failed
}
