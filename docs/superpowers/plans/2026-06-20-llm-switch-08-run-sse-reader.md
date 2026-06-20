# Task 08 — run() 接线与 SSE reader

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development 或 executing-plans。先读 [总索引](2026-06-20-llm-switch-00-index.md) Global Constraints,尤其 §4.7 错误/spawn 边界、§5.3 密钥退路。

**Goal:** 把前面各块接成真实流水线。实现:① `sse.rs` 的共享出口引擎 `run_egress`(同步 POST + 状态码校验 + 建 SSE,非 2xx 直接 `Err`;2xx 才 spawn 读取任务,逐 `data:` 喂状态机、把 `ResponseEvent` 塞进 channel,返回 `codex_api::ResponseStream`);② `lib.rs::run`(transform → 组装 `EgressCtx`(含密钥退路)→ 派发连接器);③ chat/anthropic 各自的 `run` 用 `build_*_request` + 对应 SSE 状态机调 `run_egress`。

**覆盖 spec:** §2(run 内部)、§4.7(同步建连/spawn 边界、401/403 映射)、§5.3(bearer 退路)、§2.2(构造 `codex_api::ResponseStream`)。

**Files:**
- Create: `zmod/llm-switch/src/sse.rs`
- Modify: `zmod/llm-switch/src/connector/mod.rs`(加 `SseTranslator` trait、`run_egress` 重导出、`EgressCtx` 增 `auth_fallback`)
- Modify: `zmod/llm-switch/src/connector/chat.rs` / `chat_sse.rs`(impl `SseTranslator`,填 `run`)
- Modify: `zmod/llm-switch/src/connector/anthropic.rs` / `anthropic_sse.rs`(同)
- Modify: `zmod/llm-switch/src/lib.rs`(实现 `pub async fn run`)
- Test: `zmod/llm-switch/tests/run_test.rs`(用本地 mock HTTP server 验证同步错误边界 + happy path)

**Interfaces:**
- Consumes:全部前序任务。
- Produces(Task 09 patch 依赖,**这是 core 调用的最终签名**):
  - `pub async fn run(rt: Route, request: codex_api::ResponsesApiRequest, api_auth: codex_api::SharedAuthProvider) -> Result<codex_api::ResponseStream, codex_api::ApiError>`
  - `pub(crate) trait SseTranslator: Send { fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>; fn finish(&mut self) -> Vec<codex_api::ResponseEvent>; }`
  - `pub(crate) async fn run_egress(url: String, headers: HeaderMap, body: serde_json::Value, http: reqwest::Client, translator: Box<dyn SseTranslator>) -> Result<codex_api::ResponseStream, codex_api::ApiError>`

> 与设计 §6.3 的细化:`run` 只需 `api_auth`(用于 bearer 退路),**不需** `api_provider`/`transport`/`options`——连接器用自己的 `reqwest::Client`(进程级缓存)。patch 的接管臂因此更短:`Some(rt) => codez_llm_switch::run(rt, request, api_auth.clone()).await`。原生臂仍 move `api_provider`/`api_auth`/`transport` 进 `ApiResponsesClient::new`;两臂互斥,接管臂未用到的那几个值在该臂作用域结束时正常 drop,move 合法。Task 09 据此写 patch。

---

- [ ] **Step 0: 确认 `ResponseStream` 可直接构造**

Run: `grep -n "pub struct ResponseStream" -A 8 codex-rs/codex-api/src/common.rs`
确认字段 `rx_event` / `upstream_request_id` 均为 `pub` 且无其它私有/`#[non_exhaustive]` 字段 → 可 `codex_api::ResponseStream { rx_event, upstream_request_id }` 直接构造。若发现有构造函数(如 `ResponseStream::new(rx, id)`),改用之并记录。

- [ ] **Step 1: 写失败测试(错误边界 + happy path)**

用 `wiremock`(dev-dep)起本地 HTTP mock。在 `Cargo.toml` `[dev-dependencies]` 加 `wiremock = "0.6"`、`futures = "0.3"`。

创建 `zmod/llm-switch/tests/run_test.rs`:

```rust
use codez_llm_switch::testing::{run_egress_for_test, chat_translator, dummy_headers};
use futures::StreamExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn non_2xx_returns_err_synchronously() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
        .mount(&server).await;
    let url = format!("{}/chat/completions", server.uri());
    let res = run_egress_for_test(url, dummy_headers(), serde_json::json!({}), chat_translator()).await;
    assert!(res.is_err(), "non-2xx must Err synchronously (before any spawn)");
}

#[tokio::test]
async fn happy_path_streams_events() {
    let server = MockServer::start().await;
    let sse = "data: {\"id\":\"r\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\ndata: [DONE]\n\n";
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse))
        .mount(&server).await;
    let url = format!("{}/chat/completions", server.uri());
    let stream = run_egress_for_test(url, dummy_headers(), serde_json::json!({}), chat_translator()).await.unwrap();
    let mut rx = stream.rx_event;
    let mut kinds = Vec::new();
    while let Some(item) = rx.recv().await {
        let ev = item.unwrap();
        kinds.push(format!("{ev:?}"));
    }
    assert!(kinds.iter().any(|k| k.contains("OutputTextDelta")));
    assert!(kinds.iter().any(|k| k.contains("Completed")));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd zmod/llm-switch && cargo test --test run_test`
Expected: 编译失败。

- [ ] **Step 3: 实现 `sse.rs` 的 `run_egress`**

```rust
use bytes::Bytes; // 若未引入,reqwest 已传递 bytes,可用 &[u8] 手工处理避免新依赖
use futures::StreamExt;
use reqwest::header::HeaderMap;
use tokio::sync::mpsc;
use serde_json::Value;
use codex_api::{ApiError, ResponseEvent, ResponseStream};
use crate::connector::{ConnError, SseTranslator};

pub(crate) async fn run_egress(
    url: String,
    headers: HeaderMap,
    body: Value,
    http: reqwest::Client,
    mut translator: Box<dyn SseTranslator>,
) -> Result<ResponseStream, ApiError> {
    // ---- 同步阶段(§4.7):发请求 + 状态码校验 ----
    let resp = http.post(&url).headers(headers).json(&body).send().await
        .map_err(|e| ApiError::Stream(format!("request failed: {e}")))?;
    let upstream_request_id = resp.headers().get("x-request-id")
        .and_then(|v| v.to_str().ok()).map(String::from);
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        // 第三方 401/403 映射成普通 Api 错误(非 Transport::Http UNAUTHORIZED),避免 OpenAI recovery(§4.7)
        return Err(ApiError::Api { status, message: text });
    }

    // ---- 异步阶段:仅 2xx 才 spawn ----
    let (tx, rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
    let mut byte_stream = resp.bytes_stream();
    tokio::spawn(async move {
        let mut buf = String::new();
        let mut done = false;
        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => { let _ = tx.send(Err(ApiError::Stream(format!("stream error: {e}")))).await; return; }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // 按 SSE 事件边界(空行)切分;逐 data: 处理
            while let Some(pos) = buf.find("\n\n") {
                let event_block = buf[..pos].to_string();
                buf.drain(..pos + 2);
                for line in event_block.lines() {
                    let line = line.trim_start();
                    let Some(data) = line.strip_prefix("data:") else { continue }; // 忽略 event:/id:/注释
                    let data = data.trim();
                    if data == "[DONE]" { done = true; break; }
                    if data.is_empty() { continue; }
                    let json: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(e) => { let _ = tx.send(Err(ApiError::Stream(format!("bad SSE json: {e}")))).await; return; }
                    };
                    match translator.push(&json) {
                        Ok(events) => for ev in events { if tx.send(Ok(ev)).await.is_err() { return; } }
                        Err(ce) => { let _ = tx.send(Err(ce.into())).await; return; }
                    }
                }
                if done { break; }
            }
            if done { break; }
        }
        // EOF 或 [DONE] → finish(合成 assistant message + Completed,§4.5)
        for ev in translator.finish() { if tx.send(Ok(ev)).await.is_err() { return; } }
    });

    Ok(ResponseStream { rx_event: rx, upstream_request_id })
}
```

> 依赖说明:`bytes` 通常随 reqwest 传递;若不想新增 `bytes` 直接依赖,用 `String::from_utf8_lossy(&chunk)`(`chunk: reqwest::Bytes` 实现 `AsRef<[u8]>`),如上。不要按未解码字节硬切多字节 UTF-8——SSE 帧以 `\n\n` 分隔,`from_utf8_lossy` 在帧边界足够;若担心跨 chunk 的多字节字符被 lossy 破坏,改为维护 `Vec<u8>` 缓冲、只在 `\n\n` 处 `from_utf8`。实现者二选一并记录。

- [ ] **Step 4: 定义 `SseTranslator` 并为两状态机实现**

`connector/mod.rs` 加:

```rust
pub(crate) trait SseTranslator: Send {
    fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>;
    fn finish(&mut self) -> Vec<codex_api::ResponseEvent>;
}
pub(crate) use crate::sse::run_egress;
```

`chat_sse.rs`:`impl SseTranslator for ChatSseState { fn push(&mut self, d) { self.push_chunk(d) } fn finish(&mut self){ self.finish() } }`。
`anthropic_sse.rs`:`impl SseTranslator for AnthropicSseState { fn push(&mut self, d){ self.push_event(d) } fn finish(&mut self){ self.finish() } }`。

> 注:chat 与 anthropic 都靠 reader 的 `[DONE]`/EOF 触发 `finish`;anthropic 无 `[DONE]`,靠连接关闭 EOF 触发——已在 Step 3 覆盖。

- [ ] **Step 5: 填 chat/anthropic 的 `run`**

`connector/mod.rs` 的 `EgressCtx` 增字段 `pub auth_fallback: Option<codex_api::SharedAuthProvider>`(bearer 退路,§5.3 item 3)。新增私有 helper 组装出口头:

```rust
fn egress_headers(ctx: &EgressCtx, av: Option<&str>) -> Result<reqwest::header::HeaderMap, ApiError> {
    if let Some(key) = &ctx.key {
        return crate::http::build_headers(ctx.auth, Some(key), av).map_err(|e| ApiError::InvalidRequest { message: e.to_string() });
    }
    // 无原始 key:仅 bearer 可借 codex auth(§5.3)
    match (ctx.auth, &ctx.auth_fallback) {
        (crate::config::AuthKind::Bearer, Some(provider)) => {
            let mut h = reqwest::header::HeaderMap::new();
            provider.add_auth_headers(&mut h);
            h.insert(reqwest::header::CONTENT_TYPE, reqwest::header::HeaderValue::from_static("application/json"));
            Ok(h)
        }
        _ => Err(ApiError::InvalidRequest { message: "missing API key (set key_env or auth_key)".into() }),
    }
}
```

`chat.rs::run`:

```rust
async fn run(&self, req: codex_api::ResponsesApiRequest, ctx: &EgressCtx)
    -> Result<codex_api::ResponseStream, codex_api::ApiError>
{
    let body = chat_req::build_chat_request(&req, ctx)?; // ConnError → ApiError via From
    let url = crate::http::egress_url(&ctx.base_url, crate::config::Connector::Chat, ctx.path_override.as_deref());
    let headers = super::egress_headers(ctx, None)?;
    let translator = Box::new(chat_sse::ChatSseState::default());
    super::run_egress(url, headers, body, ctx.http.clone(), translator).await
}
```

`anthropic.rs::run`:同构,`build_anthropic_request`、`Connector::Anthropic`、`egress_headers(ctx, ctx.anthropic_version.as_deref())`、`AnthropicSseState`。

- [ ] **Step 6: 实现 `lib.rs::run`**

```rust
mod sse;

pub async fn run(
    rt: Route,
    mut request: codex_api::ResponsesApiRequest,
    api_auth: codex_api::SharedAuthProvider,
) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
    // ① 变换层(v1 直通)
    let plugins = pipeline::default_plugins();
    pipeline::run_transforms(&plugins, &mut request).map_err(codex_api::ApiError::from)?;

    // 密钥
    let key = http::resolve_key(&rt.cfg).map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() })?;

    // 出口模型:config 覆盖 > 请求里的 model
    let model = rt.cfg.model.clone().unwrap_or_else(|| request.model.clone());

    let ctx = connector::EgressCtx {
        base_url: rt.cfg.base_url.clone().unwrap_or_default(), // 缺省时由 patch 传 codex provider base_url;见 Task 09 注
        model,
        auth: rt.cfg.auth,
        key,
        anthropic_version: rt.cfg.anthropic_version.clone(),
        path_override: rt.cfg.path.clone(),
        default_max_tokens: rt.cfg.default_max_tokens,
        http: shared_http_client(),
        auth_fallback: Some(api_auth),
    };
    let connector = connector::make_connector(rt.cfg.connector);
    connector.run(request, &ctx).await
}

fn shared_http_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new).clone()
}
```

> base_url 缺省:`rt.cfg.base_url` 若为 None,v1 用空串会拼出坏 URL。最简策略:**config-zmod 的 provider 必须写 base_url**(testkey 与文档示例都写了);若缺失,`run` 早返回 `ApiError::InvalidRequest{ "provider <id> missing base_url" }`。把 `unwrap_or_default()` 改成显式校验并记录。(设计 §5.2 说"缺省用 codex provider 的 base_url",但那需要 patch 额外把 provider base_url 传进 run——v1 简化为必填 base_url,在 Task 09 patch 说明里同步该简化。)

- [ ] **Step 7: testing 转发**

`lib.rs` testing 模块加:`chat_translator()`→`Box::new(ChatSseState::default()) as Box<dyn SseTranslator>`;`dummy_headers()`→ `build_headers(AuthKind::Bearer, Some("k"), None).unwrap()`;`run_egress_for_test(url, headers, body, translator)`→ 转发 `sse::run_egress(url, headers, body, reqwest::Client::new(), translator)`。

- [ ] **Step 8: 运行测试确认通过**

Run: `cd zmod/llm-switch && cargo test`
Expected:全 crate 测试 PASS(config/http/pipeline/chat_request/chat_sse/anthropic_request/anthropic_sse/run 全绿)。

- [ ] **Step 9: 提交**

```bash
git add zmod/llm-switch/src zmod/llm-switch/Cargo.toml zmod/llm-switch/tests/run_test.rs
git commit -m "feat(llm-switch): run() wiring + shared SSE egress engine (sync error boundary, spawn on 2xx)"
```
