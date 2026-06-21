//! 单行评分:内容特征 + 查询关键词加权(spec §4.4/S1)。纯静态,无 ML。
//! 供 Search/Log 决定保留哪些行。

const ERROR_KEYWORDS: &[&str] = &["error", "fail", "panic", "exception", "traceback", "fatal"];
const WARN_KEYWORDS: &[&str] = &["warn", "warning"];

/// 行得分:错误关键词 +1.0;警告 +0.5;查询词命中每个 +0.3;空行/纯符号 0。
pub fn line_score(line: &str, query: &[String]) -> f32 {
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
    for q in query {
        if lower.contains(q.as_str()) {
            score += 0.3;
        }
    }
    score
}
