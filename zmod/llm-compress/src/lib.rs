//! codez-llm-compress:在 codex LLM 请求边界压缩请求。
//! 入口 transform() 接线全部编排链:命令识别→保护门→预处理→路由压缩→CCR挂载→体积闸门。

pub mod ccr;
pub mod command;
pub mod config;
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

use crate::router::{Budget, ContentKind, ContentRouter};

fn build_router() -> ContentRouter {
    use crate::compress::{
        diff::DiffCompressor, json::JsonCompressor, log::LogCompressor,
        search::SearchCompressor, tabular::TabularCompressor, truncate::TruncateCompressor,
    };
    ContentRouter::new(vec![
        Box::new(JsonCompressor),
        Box::new(SearchCompressor),
        Box::new(DiffCompressor),
        Box::new(TabularCompressor),
        Box::new(LogCompressor),
        Box::new(TruncateCompressor),
    ])
}

/// crate 单一入口:在 LLM 请求发送边界原地压缩 request。
/// fail-open:任何环节出问题都退回原文,绝不阻断请求(返回 () 而非 Result)。
pub fn transform(request: &mut ResponsesApiRequest, _api_provider: &ApiProvider, queryid: &str) {
    let cfg = config::load();
    if !cfg.enabled {
        return;
    }
    // 一次性请求上下文
    let ctx = crate::ccr::RequestCtx {
        queryid,
        cmd_index: crate::command::index(request),
        ccr: std::cell::RefCell::new(crate::ccr::CcrRegistry::new()),
    };
    let router = build_router();

    let total_before = total_text_bytes(&request.input);
    for item in request.input.iter_mut() {
        compress_item(item, &ctx, &router, &cfg);
    }
    let total_after = total_text_bytes(&request.input);
    if total_after < total_before {
        stats::log_compression(queryid, total_before, total_after);
    }
}

fn compress_item(
    item: &mut ResponseItem,
    ctx: &crate::ccr::RequestCtx,
    router: &ContentRouter,
    cfg: &config::Config,
) {
    let call_id = match item {
        ResponseItem::FunctionCallOutput { call_id, .. } => call_id.clone(),
        ResponseItem::CustomToolCallOutput { call_id, .. } => call_id.clone(),
        _ => return,
    };
    let body = match item {
        ResponseItem::FunctionCallOutput { output, .. } => &mut output.body,
        ResponseItem::CustomToolCallOutput { output, .. } => &mut output.body,
        _ => return,
    };
    match body {
        FunctionCallOutputBody::Text(s) => compress_in_place(s, ctx, router, cfg, &call_id),
        FunctionCallOutputBody::ContentItems(items) => {
            for ci in items.iter_mut() {
                if let FunctionCallOutputContentItem::InputText { text } = ci {
                    compress_in_place(text, ctx, router, cfg, &call_id);
                }
            }
        }
    }
}

fn compress_in_place(
    s: &mut String,
    ctx: &crate::ccr::RequestCtx,
    router: &ContentRouter,
    cfg: &config::Config,
    call_id: &str,
) {
    if s.len() < cfg.per_item_min_bytes {
        return;
    }
    let cmd = ctx.cmd_index.get(call_id);
    // ② 保护门:命中即整段逐字节不变
    if crate::protect::should_protect(s, cmd, cfg) {
        return;
    }
    // ③ 预处理
    let (pre, pre_lossy) = crate::preprocess::run(s, &cfg.preprocess);
    // ④⑤ 路由压缩
    let budget = Budget { cfg, cmd };
    let mut candidate_is_json = false;
    let candidate = match router.compress_text(&pre, &budget) {
        Some((new, comp_lossy, kind)) => {
            // Structured products (Json/Toon) are NEVER decorated with a CCR
            // pointer: appending "[llm-compress: 原文 …]" would corrupt the
            // JSON, or break TOON's decodability (TOON is the model's only
            // view of this output). Only kind==Text may carry a CCR pointer,
            // and only when content was actually dropped (pre/comp lossy).
            match kind {
                ContentKind::Json => {
                    candidate_is_json = true;
                    new
                }
                ContentKind::Toon => new,
                ContentKind::Text => {
                    if pre_lossy || comp_lossy {
                        crate::ccr::attach(new, s, ctx, call_id, &cfg.ccr)
                    } else {
                        new
                    }
                }
            }
        }
        None => {
            if pre_lossy {
                crate::ccr::attach(pre, s, ctx, call_id, &cfg.ccr)
            } else {
                pre
            }
        }
    };
    // ⑥ 最终写回闸门(体积 + JSON 保卫:Json 产物必须仍可 parse)
    let json_valid = !candidate_is_json
        || serde_json::from_str::<serde_json::Value>(&candidate).is_ok();
    if candidate.len() <= s.len() && json_valid {
        *s = candidate;
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
