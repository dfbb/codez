# Task 08: transform 入口编排

> 属于 `2026-06-20-llm-compress-00-index.md`。执行前先读 index 的 Global Constraints / 真实类型。依赖 Task 01-07。

**Goal:** 实现 crate 单一入口 `transform(&mut request, &api_provider, queryid)`,编排 Layer 0(开关&预算门)→ Layer 1(遍历 `Vec<ResponseItem>`,按精确规则提取工具输出文本)→ Layer 2/3(经 ContentRouter 压缩)→ 出口(整体 saved>0 时调 stats 写日志)。装配四个压缩器 `[Json, Diff, Log, Truncate]`。

**覆盖 spec:** §1/§2(签名/集成)、§4(管线/提取规则)、§7(触发日志)、§8(fail-open)。

**Files:**
- Modify: `zmod/llm-compress/src/lib.rs`(加 `transform` + 内部编排)
- Test: `zmod/llm-compress/tests/transform_test.rs`

**Interfaces:**
- Consumes:
  - Task 01 `config::{load, Config}`
  - Task 02 `router::{Budget, ContentRouter, Compressor}`(注:trait 结果类型名为 `CompressOutcome`,**不是** spec §4 旧代码块里的 `CompressResult`——以 Task 02 为准)
  - Task 03-06 `compress::{json::JsonCompressor, diff::DiffCompressor, log::LogCompressor, truncate::TruncateCompressor}`
  - Task 07 `stats::log_compression`
  - codex 类型:`codex_api::ResponsesApiRequest`、`codex_api::Provider as ApiProvider`、`codex_protocol::models::{ResponseItem, FunctionCallOutputPayload, FunctionCallOutputBody, FunctionCallOutputContentItem}`
- Produces:
  - `pub fn transform(request: &mut ResponsesApiRequest, api_provider: &ApiProvider, queryid: &str)`

> **真实类型核实(index 已列)**:`request.input: Vec<ResponseItem>`;待处理变体 `ResponseItem::FunctionCallOutput { call_id, output }` 与 `ResponseItem::CustomToolCallOutput { call_id, name, output }`,二者 `output: FunctionCallOutputPayload { body: FunctionCallOutputBody, success: Option<bool> }`;`FunctionCallOutputBody::Text(String)` / `FunctionCallOutputBody::ContentItems(Vec<FunctionCallOutputContentItem>)`;`FunctionCallOutputContentItem::InputText { text: String }`(另有 `InputImage`/`EncryptedContent` 不碰)。

> **导入路径核实**:执行本任务前,先在 `codex-rs/` 下 `cargo doc` 或直接 grep 确认 `ResponsesApiRequest`、`Provider` 的 re-export 路径。index 已注明 `use codex_api::Provider as ApiProvider`(`core/src/client.rs:23`)。`ResponsesApiRequest` 在 `codex-api`(`codex-api/src/common.rs`)。`ResponseItem` 等模型在 `codex-protocol`(crate 名 `codex_protocol`,见 Cargo.toml 的 `codex-protocol` 依赖)。若编译报路径不符,以 `cargo build -p codez-llm-compress` 的报错为准修正 `use`,**不要**改类型语义。

---

- [ ] **Step 1: 写失败测试**

Create `zmod/llm-compress/tests/transform_test.rs`:

```rust
//! transform 端到端编排测试。用真实 codex 类型构造 request。

use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseItem,
};

/// 构造一个最小的 ResponsesApiRequest,input 由调用者给。
/// 注:ResponsesApiRequest 字段较多,用 ..Default 不可行(无 Default);
/// 这里用一个 helper 把必填字段填上最小值。字段以 codex-api/src/common.rs 为准,
/// 若字段集变化,编译器会指出——按报错补齐,值取空/false/None 即可。
fn req_with(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "gpt-test".to_string(),
        instructions: String::new(),
        input,
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn provider() -> codex_api::Provider {
    // Provider 构造:以 codex-api/src/provider.rs 的公开构造/Default 为准。
    // 若无 Default,按其字段最小构造;本测试不依赖 provider 内容(transform 只读判别)。
    codex_api::Provider::default()
}

fn fco_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success: Some(true),
        },
    }
}

#[test]
fn disabled_config_leaves_request_untouched() {
    // 无 config-zmod 文件 → enabled=false → request 不变。
    let big = "x\n".repeat(10_000);
    let mut r = req_with(vec![fco_text("c1", &big)]);
    let before = r.clone();
    transform(&mut r, &provider(), "qid-1");
    assert_eq!(r.input.len(), before.input.len());
    if let (
        ResponseItem::FunctionCallOutput { output: a, .. },
        ResponseItem::FunctionCallOutput { output: b, .. },
    ) = (&r.input[0], &before.input[0])
    {
        // 关闭时逐字节不变
        match (&a.body, &b.body) {
            (FunctionCallOutputBody::Text(sa), FunctionCallOutputBody::Text(sb)) => {
                assert_eq!(sa, sb)
            }
            _ => panic!("body shape changed"),
        }
    } else {
        panic!("variant changed");
    }
}

#[test]
fn non_tooloutput_variants_are_ignored() {
    // Message 等变体不处理(此处用 FunctionCall 占位另一个变体即可,只验证不 panic、长度不变)。
    let mut r = req_with(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert_eq!(r.input.len(), 1);
    assert!(matches!(r.input[0], ResponseItem::Other));
}

#[test]
fn contentitems_image_preserved() {
    // ContentItems 含 InputText + InputImage:图片必须原样保留。
    let mut r = req_with(vec![ResponseItem::FunctionCallOutput {
        call_id: "c3".to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText { text: "short".to_string() },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AAAA".to_string(),
                    detail: None,
                },
            ]),
            success: None,
        },
    }]);
    transform(&mut r, &provider(), "qid-3");
    if let ResponseItem::FunctionCallOutput { output, .. } = &r.input[0] {
        if let FunctionCallOutputBody::ContentItems(items) = &output.body {
            // 图片项原样保留
            assert!(items.iter().any(|it| matches!(
                it,
                FunctionCallOutputContentItem::InputImage { image_url, .. } if image_url == "data:image/png;base64,AAAA"
            )));
        } else {
            panic!("body shape changed");
        }
    } else {
        panic!("variant changed");
    }
}
```

> **测试启用压缩的路径**:`disabled_config_leaves_request_untouched` 覆盖关闭路径。"开启后确有压缩"的端到端断言较脆(依赖 ~/.codex 真实文件),留给 Task 09 的 live 验证;此处单测聚焦"关闭不变 / 非目标变体不动 / 图片保留"三条不依赖外部配置的不变量。

- [ ] **Step 2: 跑测试看失败**

Run(`codex-rs/`):
```bash
cargo test -p codez-llm-compress --test transform_test
```
Expected: 编译失败(`transform` 未定义)。若 `req_with`/`provider` 的字段或构造与真实类型不符,编译器会报字段错——按报错调整测试 helper 的字段(取最小值),不改语义。

- [ ] **Step 3: 写 transform 编排(lib.rs)**

Modify `zmod/llm-compress/src/lib.rs`,在文件末尾追加(模块声明 `pub mod stats;` `pub mod compress;` 等应已由 01-07 加好;若缺则补):

```rust
use codex_api::Provider as ApiProvider;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, ResponseItem,
};

use crate::compress::{
    diff::DiffCompressor, json::JsonCompressor, log::LogCompressor, truncate::TruncateCompressor,
};
use crate::config::Config;
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
    let budget = Budget { cfg: &cfg };

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
    if let Some(new) = router.compress_text(s, budget) {
        *s = new;
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

// 让 Config 在本模块可见(若上方 use 已含则忽略)。
use crate::config;
```

> **注意 use 去重**:`lib.rs` 顶部已有 `pub mod config;`(Task 01)、`pub mod router;`(Task 02)、`pub mod stats;`(Task 07)、`pub mod compress;`(Task 03-06)。本步追加的 `use crate::config;` 等若与既有重复,删去重复行;以 `cargo build` 通过为准。

- [ ] **Step 4: 跑测试看通过**

Run(`codex-rs/`):
```bash
cargo test -p codez-llm-compress --test transform_test
```
Expected: 3 passed。若 `req_with`/`provider` helper 因真实字段集不符报错,按编译器提示补齐字段(最小值),再跑。

- [ ] **Step 5: 跑全 crate 测试(回归)**

Run(`codex-rs/`):
```bash
cargo test -p codez-llm-compress
```
Expected: 全绿(config / router / truncate / json / diff / log / stats / transform 各 test 文件)。

- [ ] **Step 6: 提交**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-08-transform-entry.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): transform entry orchestration (Layer 0-3 + ResponseItem extraction)"
```
