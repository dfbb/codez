//! 从 request 提取"最后一条 user 消息"的关键词,供 score 做查询加权(spec §4.2/S2)。

use codex_api::ResponsesApiRequest;
use codex_protocol::models::{ContentItem, ResponseItem};

const MAX_TERMS: usize = 32;
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "are", "was",
    "you", "your", "but", "not", "all", "can", "has", "have", "will", "what",
];

/// 从后往前找第一条 role=="user" 的 Message,取其 InputText 文本,分词成关键词。
/// 找不到 → 空 Vec(评分退化为纯内容特征,不报错)。
pub fn extract(request: &ResponsesApiRequest) -> Vec<String> {
    let text: Option<String> = request.input.iter().rev().find_map(|item| match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            let mut s = String::new();
            for c in content {
                if let ContentItem::InputText { text } = c {
                    s.push_str(text);
                    s.push(' ');
                }
            }
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        _ => None,
    });
    match text {
        Some(t) => tokenize(&t),
        None => Vec::new(),
    }
}

fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() <= 2 {
            continue;
        }
        let w = raw.to_lowercase();
        if STOPWORDS.contains(&w.as_str()) {
            continue;
        }
        if seen.insert(w.clone()) {
            out.push(w);
            if out.len() >= MAX_TERMS {
                break;
            }
        }
    }
    out
}
