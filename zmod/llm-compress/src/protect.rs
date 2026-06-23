//! Error output protection gate (spec §4.5/C2): error/exception and < threshold → entire segment not compressed.
//! Highest priority: determined before all preprocessing, if matched the entire segment remains byte-for-byte unchanged (spec §2/§4.5 #7).

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

/// Text contains strong error indicators and len < cfg.protect.error_max_bytes → true (entire segment not compressed).
/// error_max_bytes==0 → protection disabled, always false.
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
    // Command hint assistance: when test runner output contains fail/error, increase protection inclination.
    let test_failed = cmd.is_some_and(|c| c.is_test_runner())
        && (lower.contains("fail") || lower.contains("error"));
    has_error || test_failed
}
