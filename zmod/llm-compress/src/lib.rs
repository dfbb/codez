//! codez-llm-compress:在 codex LLM 请求边界压缩请求。
//! 入口 transform() 在 Task 08 加入;本任务先建 config 地基。

pub mod command;
pub mod config;
pub mod query;
pub mod router;
pub mod score;
pub mod compress;
pub mod stats;
pub mod protect;
pub mod preprocess;

/// 是否启用压缩(读 ~/.codex/config-zmod.toml 的 [llm_compress].enabled)。
pub fn enabled() -> bool {
    config::load().enabled
}

use codex_api::Provider as ApiProvider;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, ResponseItem,
};

use crate::compress::{
    diff::DiffCompressor, json::JsonCompressor, log::LogCompressor, truncate::TruncateCompressor,
};
use crate::router::{Budget, ContentRouter};

/// 装配四压缩器,固定优先级 Json → Diff → Log → Truncate(Truncate 兜底)。
fn build_router() -> ContentRouter {
    ContentRouter::new(vec![
        Box::new(JsonCompressor),
        Box::new(DiffCompressor),
        Box::new(LogCompressor),
        Box::new(TruncateCompressor),
    ])
}

/// crate 单一入口:在 LLM 请求发送边界原地压缩 request。
/// fail-open:任何环节出问题都退回原文,绝不阻断请求(返回 () 而非 Result)。
pub fn transform(request: &mut ResponsesApiRequest, _api_provider: &ApiProvider, queryid: &str) {
    let cfg = config::load();

    // Layer 0:开关
    if !cfg.enabled {
        return;
    }

    // Layer 0:预算门——input 文本总量低于 min_total_bytes 不折腾
    let total_before = total_text_bytes(&request.input);
    if total_before < cfg.min_total_bytes {
        return;
    }

    let router = build_router();
    let empty_query: Vec<String> = Vec::new();
    let budget = Budget { cfg: &cfg, cmd: None, query: &empty_query };

    // Layer 1:遍历 input,只处理两个工具输出变体,逐文本片段压缩
    for item in request.input.iter_mut() {
        compress_item(item, &router, &budget, cfg.per_item_min_bytes);
    }

    // 出口:整体确有压缩才写日志
    let total_after = total_text_bytes(&request.input);
    if total_after < total_before {
        stats::log_compression(queryid, total_before, total_after);
    }
}

/// 对单个 ResponseItem:仅 FunctionCallOutput / CustomToolCallOutput 的 body 文本被压缩。
fn compress_item(
    item: &mut ResponseItem,
    router: &ContentRouter,
    budget: &Budget,
    per_item_min_bytes: usize,
) {
    let body = match item {
        ResponseItem::FunctionCallOutput { output, .. } => &mut output.body,
        ResponseItem::CustomToolCallOutput { output, .. } => &mut output.body,
        _ => return, // 其它变体一律不动
    };

    match body {
        FunctionCallOutputBody::Text(s) => {
            compress_in_place(s, router, budget, per_item_min_bytes);
        }
        FunctionCallOutputBody::ContentItems(items) => {
            for ci in items.iter_mut() {
                // 仅压 InputText.text;InputImage / EncryptedContent 不读不改
                if let FunctionCallOutputContentItem::InputText { text } = ci {
                    compress_in_place(text, router, budget, per_item_min_bytes);
                }
            }
        }
    }
}

/// 单个文本片段:低于阈值跳过;否则经 router 压缩,成功则原地替换。
fn compress_in_place(s: &mut String, router: &ContentRouter, budget: &Budget, min_bytes: usize) {
    if s.len() < min_bytes {
        return;
    }
    if let Some((new, _lossy, _kind)) = router.compress_text(s, budget) {
        // router 已保证 saved_bytes>0;此处 `<=` 是防御性二次体积闸门,
        // 防止压缩器被直接调用绕过 router 时破坏「压后≤压前」不变量。
        if new.len() <= s.len() {
            *s = new;
        }
    }
}

/// 统计 input 中所有"可压缩文本片段"的字节总和(与压缩作用对象一致)。
fn total_text_bytes(input: &[ResponseItem]) -> usize {
    let mut total = 0usize;
    for item in input {
        let body = match item {
            ResponseItem::FunctionCallOutput { output, .. } => &output.body,
            ResponseItem::CustomToolCallOutput { output, .. } => &output.body,
            _ => continue,
        };
        match body {
            FunctionCallOutputBody::Text(s) => total += s.len(),
            FunctionCallOutputBody::ContentItems(items) => {
                for ci in items {
                    if let FunctionCallOutputContentItem::InputText { text } = ci {
                        total += text.len();
                    }
                }
            }
        }
    }
    total
}
