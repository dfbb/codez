# Task 08 — run() Wiring and SSE reader

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the [master index](2026-06-20-llm-switch-00-index.md) Global Constraints, especially §4.7 error/spawn boundary and §5.3 key fallback.

**Goal:** Wire the preceding blocks into a real pipeline. Implement: ① the shared egress engine `run_egress` in `sse.rs` (synchronous POST + status-code validation + SSE setup; non-2xx returns `Err` immediately, only 2xx spawns the reader task, feeds each `data:` line into the state machine, pushes `ResponseEvent`s into a channel, and returns a `codex_api::ResponseStream`); ② `lib.rs::run` (transform → assemble `EgressCtx` (including key fallback) → dispatch to the connector); ③ chat/anthropic each implement their own `run` using `build_*_request` + the corresponding SSE state machine to call `run_egress`.

**Spec coverage:** §2 (run internals), §4.7 (synchronous connection setup / spawn boundary, 401/403 mapping), §5.3 (bearer fallback), §2.2 (constructing `codex_api::ResponseStream`).

**Files:**
- Create: `zmod/llm-switch/src/sse.rs`
- Modify: `zmod/llm-switch/src/connector/mod.rs` (add `SseTranslator` trait, re-export `run_egress`, add `auth_fallback` to `EgressCtx`)
- Modify: `zmod/llm-switch/src/connector/chat.rs` / `chat_sse.rs` (impl `SseTranslator`, fill in `run`)
- Modify: `zmod/llm-switch/src/connector/anthropic.rs` / `anthropic_sse.rs` (same)
- Modify: `zmod/llm-switch/src/lib.rs` (implement `pub async fn run`)
- Test: `zmod/llm-switch/tests/run_test.rs` (use a local mock HTTP server to verify the synchronous error boundary + happy path)

**Interfaces:**
- Consumes: all preceding tasks.
- Produces (depended on by the Task 09 patch, **this is the final signature called by core**):
  - `pub async fn run(rt: Route, request: codex_api::ResponsesApiRequest, api_auth: codex_api::SharedAuthProvider) -> Result<codex_api::ResponseStream, codex_api::ApiError>`
  - `pub(crate) trait SseTranslator: Send { fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>; fn finish(&mut self) -> Vec<codex_api::ResponseEvent>; }`
  - `pub(crate) async fn run_egress(url: String, headers: HeaderMap, body: serde_json::Value, http: reqwest::Client, translator: Box<dyn SseTranslator>) -> Result<codex_api::ResponseStream, codex_api::ApiError>`

> Refinement over design §6.3: `run` only needs `api_auth` (for the bearer fallback) and does **not** need `api_provider`/`transport`/`options`—the connector uses its own `reqwest::Client` (process-level cached). The patch's takeover arm is therefore shorter: `Some(rt) => codez_llm_switch::run(rt, request, api_auth.clone()).await`. The native arm still moves `api_provider`/`api_auth`/`transport` into `ApiResponsesClient::new`; the two arms are mutually exclusive, and the values the takeover arm doesn't use are dropped normally when that arm's scope ends, so the move is valid. Task 09 writes the patch accordingly.

---

- [ ] **Step 0: Confirm `ResponseStream` can be constructed directly**

Run: `grep -n "pub struct ResponseStream" -A 8 codex-rs/codex-api/src/common.rs`
Confirm that fields `rx_event` / `upstream_request_id` are both `pub` and that there are no other private / `#[non_exhaustive]` fields → it can be constructed directly as `codex_api::ResponseStream { rx_event, upstream_request_id }`. If you find a constructor (e.g. `ResponseStream::new(rx, id)`), use it instead and record it.

- [ ] **Step 1: Write the failing tests (error boundary + happy path)**

Use `wiremock` (dev-dep) to start a local HTTP mock. Add `wiremock = "0.6"` and `futures = "0.3"` under `[dev-dependencies]` in `Cargo.toml`.

Create `zmod/llm-switch/tests/run_test.rs`:

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

- [ ] **Step 2: Run and confirm failure**

Run: `cd zmod/llm-switch && cargo test --test run_test`
Expected: compilation failure.

- [ ] **Step 3: Implement `run_egress` in `sse.rs`**

```rust
use bytes::Bytes; // if not already imported, reqwest already passes bytes; you can handle &[u8] manually to avoid a new dependency
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
    // ---- Synchronous phase (§4.7): send request + validate status code ----
    let resp = http.post(&url).headers(headers).json(&body).send().await
        .map_err(|e| ApiError::Stream(format!("request failed: {e}")))?;
    let upstream_request_id = resp.headers().get("x-request-id")
        .and_then(|v| v.to_str().ok()).map(String::from);
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        // Map third-party 401/403 to a plain Api error (not Transport::Http UNAUTHORIZED) to avoid OpenAI recovery (§4.7)
        return Err(ApiError::Api { status, message: text });
    }

    // ---- Asynchronous phase: spawn only on 2xx ----
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
            // Split on SSE event boundaries (blank lines); process each data: line
            while let Some(pos) = buf.find("\n\n") {
                let event_block = buf[..pos].to_string();
                buf.drain(..pos + 2);
                for line in event_block.lines() {
                    let line = line.trim_start();
                    let Some(data) = line.strip_prefix("data:") else { continue }; // ignore event:/id:/comments
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
        // EOF or [DONE] → finish (synthesize assistant message + Completed, §4.5)
        for ev in translator.finish() { if tx.send(Ok(ev)).await.is_err() { return; } }
    });

    Ok(ResponseStream { rx_event: rx, upstream_request_id })
}
```

> Dependency note: `bytes` is usually passed through by reqwest; if you don't want to add a direct `bytes` dependency, use `String::from_utf8_lossy(&chunk)` (`chunk: reqwest::Bytes` implements `AsRef<[u8]>`), as shown above. Don't slice multi-byte UTF-8 on undecoded byte boundaries—SSE frames are separated by `\n\n`, and `from_utf8_lossy` is sufficient at frame boundaries; if you're worried about multi-byte characters spanning chunks being corrupted by lossy decoding, maintain a `Vec<u8>` buffer instead and only `from_utf8` at the `\n\n` boundaries. The implementer picks one and records it.

- [ ] **Step 4: Define `SseTranslator` and implement it for both state machines**

Add to `connector/mod.rs`:

```rust
pub(crate) trait SseTranslator: Send {
    fn push(&mut self, data: &serde_json::Value) -> Result<Vec<codex_api::ResponseEvent>, ConnError>;
    fn finish(&mut self) -> Vec<codex_api::ResponseEvent>;
}
pub(crate) use crate::sse::run_egress;
```

`chat_sse.rs`: `impl SseTranslator for ChatSseState { fn push(&mut self, d) { self.push_chunk(d) } fn finish(&mut self){ self.finish() } }`.
`anthropic_sse.rs`: `impl SseTranslator for AnthropicSseState { fn push(&mut self, d){ self.push_event(d) } fn finish(&mut self){ self.finish() } }`.

> Note: both chat and anthropic rely on the reader's `[DONE]`/EOF to trigger `finish`; anthropic has no `[DONE]` and relies on connection-close EOF to trigger it—already covered in Step 3.

- [ ] **Step 5: Fill in the chat/anthropic `run`**

Add the field `pub auth_fallback: Option<codex_api::SharedAuthProvider>` to `EgressCtx` in `connector/mod.rs` (bearer fallback, §5.3 item 3). Add a private helper to assemble the egress headers:

```rust
fn egress_headers(ctx: &EgressCtx, av: Option<&str>) -> Result<reqwest::header::HeaderMap, ApiError> {
    if let Some(key) = &ctx.key {
        return crate::http::build_headers(ctx.auth, Some(key), av).map_err(|e| ApiError::InvalidRequest { message: e.to_string() });
    }
    // No original key: only bearer may borrow codex auth (§5.3)
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

`anthropic.rs::run`: same structure, with `build_anthropic_request`, `Connector::Anthropic`, `egress_headers(ctx, ctx.anthropic_version.as_deref())`, and `AnthropicSseState`.

- [ ] **Step 6: Implement `lib.rs::run`**

```rust
mod sse;

pub async fn run(
    rt: Route,
    mut request: codex_api::ResponsesApiRequest,
    api_auth: codex_api::SharedAuthProvider,
) -> Result<codex_api::ResponseStream, codex_api::ApiError> {
    // ① Transform layer (v1 pass-through)
    let plugins = pipeline::default_plugins();
    pipeline::run_transforms(&plugins, &mut request).map_err(codex_api::ApiError::from)?;

    // Key
    let key = http::resolve_key(&rt.cfg).map_err(|e| codex_api::ApiError::InvalidRequest { message: e.to_string() })?;

    // Egress model: config override > model in the request
    let model = rt.cfg.model.clone().unwrap_or_else(|| request.model.clone());

    let ctx = connector::EgressCtx {
        base_url: rt.cfg.base_url.clone().unwrap_or_default(), // when absent, the patch passes the codex provider base_url; see Task 09 note
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

> base_url default: if `rt.cfg.base_url` is None, v1 using an empty string would build a broken URL. Simplest strategy: **the config-zmod provider must specify base_url** (both testkey and the doc examples do); if it's missing, `run` returns early with `ApiError::InvalidRequest{ "provider <id> missing base_url" }`. Change `unwrap_or_default()` into explicit validation and record it. (Design §5.2 says "default to the codex provider's base_url," but that would require the patch to additionally pass the provider base_url into run—v1 simplifies this to a mandatory base_url; note this simplification in the Task 09 patch documentation.)

- [ ] **Step 7: testing forwarding**

Add to the `testing` module in `lib.rs`: `chat_translator()` → `Box::new(ChatSseState::default()) as Box<dyn SseTranslator>`; `dummy_headers()` → `build_headers(AuthKind::Bearer, Some("k"), None).unwrap()`; `run_egress_for_test(url, headers, body, translator)` → forward to `sse::run_egress(url, headers, body, reqwest::Client::new(), translator)`.

- [ ] **Step 8: Run the tests and confirm they pass**

Run: `cd zmod/llm-switch && cargo test`
Expected: all crate tests PASS (config/http/pipeline/chat_request/chat_sse/anthropic_request/anthropic_sse/run all green).

- [ ] **Step 9: Commit**

```bash
git add zmod/llm-switch/src zmod/llm-switch/Cargo.toml zmod/llm-switch/tests/run_test.rs
git commit -m "feat(llm-switch): run() wiring + shared SSE egress engine (sync error boundary, spawn on 2xx)"
```
