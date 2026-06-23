//! Line scoring: content features only (error/warn keywords). Pure static, no ML.
//! Used by Search/Log to decide which lines to keep.

const ERROR_KEYWORDS: &[&str] = &["error", "fail", "panic", "exception", "traceback", "fatal"];
const WARN_KEYWORDS: &[&str] = &["warn", "warning"];

/// Line score: error keywords +1.0; warnings +0.5; blank/symbol-only 0.
pub fn line_score(line: &str) -> f32 {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return 0.0;
    }
    let lower = line.to_lowercase();
    let mut score = 0.0_f32;
    if ERROR_KEYWORDS.iter().any(|k| lower.contains(k)) {
        score += 1.0;
    } else if WARN_KEYWORDS.iter().any(|k| lower.contains(k)) {
        score += 0.5;
    }
    score
}
