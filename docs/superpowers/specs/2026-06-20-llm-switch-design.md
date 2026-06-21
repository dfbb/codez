# zmod/llm-switch — Design Document

Date: 2026-06-20
Status: Design approved, implementation plan pending

## 1. Goals and Background

Enable codex to connect to non-OpenAI models such as Anthropic and DeepSeek. codex **always speaks only the Responses protocol** to `base_url` (the `WireApi` enum currently only has `Responses`; `Chat` has been permanently removed), so connecting to these upstreams requires protocol translation between codex and the real upstream.

This project = codez's first zmod, crate name `codez-llm-switch`, directory `zmod/llm-switch`, corresponding patch `patches/llm-switch.patch` (following codez's zmod ↔ patch naming convention).

### 1.1 Key Decisions (Confirmed)

| Decision Point | Choice |
|---|---|
| Delivery target | End-to-end usable: once codex is configured, anthropic/deepseek actually work |
| Integration form | **Directly take over codex's LLM API layer (in-process), no separate proxy process** |
| Connection-layer engine | **Thin connector**, operating on codex's native types, translation rules referencing `../3rd/proxy/llm-rosetta` |
| v1 protocol coverage | Anthropic Messages, Chat Completions (deepseek/OpenAI-compatible), Responses passthrough |
| Future extension | LLM query compression (rtk/headroom-like), attached to pipeline stage ① as a transform plugin |
| Out of scope (v1) | Google GenAI, WebSocket transport, model-initiated MCP fetch-back |

### 1.2 Relationship to rust-llm-proxy / llm-rosetta

`../3rd/proxy/rust-llm-proxy` is a Rust port of llm-rosetta, but it is only a **pure conversion library** (and only implements the OpenAiChat converter, with no network layer). This design **does not port its generic IR-hub wholesale**: because compression happens at pipeline stage ① using codex's native types and is protocol-agnostic, the connection layer only needs one-directional translation of `Responses→target` (outbound) + `target SSE→Responses` (inbound), so the generic N×N hub is unnecessary. A thin connector means less code, is closest to codex types, and is most friendly to upstream rebases; translation correctness is ensured by cross-checking against the converters in `../3rd/proxy/llm-rosetta` plus golden tests.

## 2. Architecture and Integration Points

An in-process two-layer pipeline, attached at the HTTP send boundary of the codex client (`stream_responses_api` in `core/src/client.rs`).

```
core/src/client.rs  stream_responses_api()  —— inside the loop, after the request is constructed
  │ assembled ResponsesApiRequest (codex native)
  ▼
let stream_result: Result<codex_api::ResponseStream, ApiError> =
  match codez_llm_switch::route(&state.model_provider_id) {
    None => {                                                          // native path: telemetry chain preserved
        let client = ApiResponsesClient::new(transport, api_provider, api_auth)
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));   // ★ must not drop
        client.stream_request(request, options).await
    }
    Some(rt) =>                                                        // takeover path (owned args, see below)
        codez_llm_switch::run(rt, request, api_provider, api_auth, transport, options).await,
  };
// the two arms are mutually exclusive; moving api_provider/api_auth/transport into each branch is legal, no clone/borrow needed
  ▼
// ↓ reuse the same downstream handling already in stream_responses_api, no separate early return:
match stream_result {
  Ok(stream)            => map_response_stream(stream, telemetry, trace_attempt),  // LastResponse / cancellation / telemetry
  Err(401 Unauthorized) => continue after handle_unauthorized(...),                // native recovery unchanged
  Err(err)              => map_api_error(err) + trace_attempt.record_failed(...),  // native failure recording unchanged
}
```

Inside `codez_llm_switch::run`: ① `TransformPlugin[]` transforms (operating on codex's native `ResponsesApiRequest`; v1 passthrough, with a compressor attached later) → ② `Connector` egress translation + HTTP/SSE → spawns a task to read the upstream SSE, translate it into `ResponseEvent`, push into a channel, and **returns `codex_api::ResponseStream`**.

**Layering rationale**: Compression cares about "what the model actually sees" — the `ResponsesApiRequest` codex has already assembled (the `content` of each `ResponseItem` / `FunctionCallOutput.output`) — which is independent of whether the upstream is anthropic or deepseek, so it should be compressed exactly once in the Responses semantic space (pipeline ①). Protocol translation is an egress concern (pipeline ②). The two concerns live in two separate layers.

### 2.1 Routing Key: `model_provider_id` (Correction: name cannot be used)

`ModelProviderInfo` only has `name` (a display name), and **does not have** the `[model_providers.<id>]` key; `ModelClient::new` also only takes `ModelProviderInfo`, not the id. The config key lives in `Config.model_provider_id` (`core/src/config/mod.rs:631`).

Correction: the patch adds a `model_provider_id: String` parameter to `ModelClient::new` (passed in by the caller from `Config.model_provider_id`), stored into `ModelClientState.model_provider_id`, and routing uses it. **Do not fall back to `name` or `base_url`** (name is a free-form display name, and under takeover semantics base_url need not equal the real upstream — both are unstable/non-unique).

### 2.2 Stream Type: Return `codex_api::ResponseStream`, mapped by core (Correction: do not construct core's private stream)

The `ResponseStream` fields at `core/src/client_common.rs:103` are `pub(crate)` (`rx_event` / `consumer_dropped`), so zmod **cannot** construct it.

Correction: `codez_llm_switch::run` returns `Result<codex_api::ResponseStream, ApiError>` — **exactly the same type** as `ApiResponsesClient::stream_request`. The core side continues to use the existing `map_response_stream(stream, …)` (`client.rs:1758`) to wrap it into a core `ResponseStream`, getting `LastResponse` tracking, `consumer_dropped` cancellation, and stream telemetry for free. `map_response_events<S>` (`client.rs:1779`) already accepts any `Stream<Item = Result<ResponseEvent, ApiError>>`, so even if the connector later wants to return a different stream, it only needs to satisfy this bound.

### 2.3 Integration is not an early return, but joining the existing `stream_result` flow (Correction)

The `stream_responses_api` loop contains unauthorized recovery, `map_api_error`, `inference_trace_attempt.record_failed`, and `map_response_stream`. A direct `return` would bypass all of these.

Correction: make "native `client.stream_request(...)`" and "`llm_switch::run(...)`" into a two-way choice that both produce **the same `Result<codex_api::ResponseStream, ApiError>`**, with both results falling into the **same** `match stream_result { … }`. The connector maps its own transport/translation errors to `ApiError` (`ApiError::Transport(TransportError::Http{status,…})`, etc.), so the existing match arms handle them correctly. Note: OpenAI-specific 401 recovery will not trigger for anthropic/deepseek (their errors go through the generic failure arm), which is acceptable — the recovery logic is preserved, it just doesn't get hit.

### 2.4 Telemetry Boundary (Correction: the takeover path does not wire up codex-api-layer request/SSE telemetry)

The native path attaches **request-level + SSE-level** telemetry on `ApiResponsesClient::new(...).with_telemetry(Some(request_telemetry), Some(sse_telemetry))` (`client.rs:1324`). These two telemetry objects are bound to `ApiResponsesClient` (the Responses endpoint); the takeover path uses **its own** HTTP/SSE client and **cannot** reuse them. Explicit boundary:

- **Preserved** (effective on both paths, outside `stream_responses_api`, independent of the connector): `inference_trace_attempt` (`record_started`/`record_failed`/`record_cancelled`), and `map_response_stream`'s `LastResponse` / cancellation / stream telemetry.
- **Not wired up in v1** (a known gap on the takeover path; **we do not falsely claim "telemetry unchanged"**): the codex-api-layer `request_telemetry` / `sse_telemetry`. The connector may record minimal metrics within the crate (status code, time-to-first-byte), but does not replicate codex-api's telemetry suite.
- The native path (responses passthrough via the native client, or routing not matched) **fully preserves** `.with_telemetry(...)`.

**The connector consumes codex's native types directly**: `codez-llm-switch` depends on `codex-api` (`ResponsesApiRequest`/`ResponseEvent`/`ResponseStream`) and `codex-protocol` (`ResponseItem`). This naturally stays "compatible with codex's latest third-party support" — if codex changes the Responses types, it surfaces at compile time.

## 3. Crate Module Layout

```
zmod/llm-switch/
  Cargo.toml                 # name = codez-llm-switch; depends on codex-api / codex-protocol / reqwest / tokio / serde
  src/
    lib.rs                   # run(): pipeline entry; enabled()/route() routing decisions
    config.rs                # reads the [llm-switch] section of ~/.codex/config-zmod.toml
    pipeline.rs              # TransformPlugin trait + ordered execution (v1 registration point only)
    transform/
      mod.rs                 # future compressor lands here; empty in v1
    connector/
      mod.rs                 # Connector trait + factory (chat / anthropic only); responses is not here (uses native branch, §4.1)
      chat.rs                # Responses ⇄ Chat Completions (deepseek / OpenAI-compatible)
      anthropic.rs           # Responses ⇄ Anthropic Messages
    sse.rs                   # upstream SSE reading + per-event streaming translation fed to the connector
    http.rs                  # egress HTTP client + auth header shaping (Bearer / x-api-key)
  tests/
    testkey.toml             # real key for live runs (gitignored, not committed)
    fixtures/                # sample JSON taken from llm-rosetta
    chat_roundtrip.rs
    anthropic_roundtrip.rs

patches/llm-switch.patch     # see §6
```

## 4. Connector Translation Details

Common contract:

```rust
trait Connector {
    // returns a stream of the same type as ApiResponsesClient::stream_request, handed to core's map_response_stream for wrapping
    async fn run(&self, req: ResponsesApiRequest, ctx: &EgressCtx)
        -> Result<codex_api::ResponseStream, ApiError>;
}
```

`EgressCtx` carries base_url, auth, the reqwest transport, target model, and config-zmod override items.

**`run`'s error/spawn boundary (Correction, see §4.7)**: `run` must **synchronously** complete the HTTP request + status-code check + SSE response establishment; **a non-2xx (connection/auth failure) directly `return Err(ApiError)`** (falling into the outer `match stream_result`, going through `map_api_error` + `record_failed`); **only after obtaining a 2xx SSE does it `spawn` the reader task**, and only stream errors within the task get `tx.send(Err(ApiError…))` (going through `map_response_events`'s stream error handling).

The reference baselines for field-mapping correctness are in §8 (chat uses rust-llm-proxy; anthropic uses the llm-rosetta Python converter or a self-built fixture); **do not** vaguely look for "the corresponding converter".

### 4.0 `ResponseItem` Variant Disposition Strategy (Correction: cover the full set, eliminate silent dropping)

`ResponseItem` actually has 16 variants (`protocol/src/models.rs:919`). The connector must have an **explicit** disposition for each variant: `translate` (translate into the target protocol) / `drop on egress` (don't send upstream, but **don't touch** codex's local history — the connector only constructs a request copy, and never modifies the items codex stores) / `hard fail` (return `ApiError`, never silently swallow, otherwise the tool chain/context is broken).

| ResponseItem variant | chat | anthropic | Notes |
|---|---|---|---|
| `Message` | translate | translate | text/multimodal content; image input see §7 field grading |
| `AgentMessage` | pure `InputText` downgraded to assistant text; containing `EncryptedContent` → **hard fail** | same | `AgentMessageInputContent` has two variants `InputText`/`EncryptedContent` (`protocol/src/models.rs`). The latter cannot be read by a non-Responses upstream — hard fail rather than silently drop (dropping it would change the model-visible assistant history) |
| `FunctionCall` | translate (`tool_calls`); **`namespace.is_some()` → hard fail** | translate (`tool_use`, arguments string→object); **`namespace.is_some()` → hard fail** | a standard function tool call, **the only tool-call form supported in v1**; `FunctionCall.namespace: Option<String>` is a codex namespace tool with no reversible expression in the target protocol — v1 hard fails (consistent with the namespace tool-definition hard fail in §4.0b) |
| `FunctionCallOutput` | see §4.6 (includes `success` status) | see §4.6 | payload = `body` (text or `ContentItems`, possibly `InputImage`/`EncryptedContent`) + `success: Option<bool>`; cannot map body alone |
| `CustomToolCall` / `CustomToolCallOutput` | **hard fail** | **hard fail** | freeform/custom tools serialize to `type:"custom"`, with no standard JSON-schema parameters, which a Chat/Anthropic function tool may not be able to express; **not supported in v1, hard fail** (see §4.0b) |
| `LocalShellCall` | **hard fail** | **hard fail** | a provider/native tool history item, **not equivalent** to an ordinary function call; translating it into a function would make the model keep referring to a tool the target provider does not have. v1 hard fail |
| `ToolSearchCall` / `ToolSearchOutput` | **hard fail** | **hard fail** | same as above, provider/native tools, v1 hard fail |
| `WebSearchCall` | **hard fail** | **hard fail** | same as above, v1 hard fail |
| `ImageGenerationCall` | **hard fail** | **hard fail** | same as above, v1 hard fail |
| `Reasoning` | drop on egress | drop on egress | see §4.4 (encrypted_content cannot be sent to a non-Responses upstream) |
| `Compaction` / `ContextCompaction` | **hard fail** | **hard fail** | both carry `encrypted_content` (`Compaction.encrypted_content: String`, `ContextCompaction.encrypted_content: Option<String>`, verified), carrying model-visible compacted history. A non-Responses upstream cannot read it, and **silently dropping would change model-visible history**, so hard fail (equivalent to: v1 does not support histories containing these items) |
| `CompactionTrigger` | drop on egress | drop on egress | only `metadata`, no `encrypted_content`/body (verified), a pure trigger marker, safe to drop |
| `Other` | **hard fail** | **hard fail** | unknown variant (`#[serde(other)]`), cannot be safely translated |

> Principle: **v1 tool capabilities only support standard `FunctionCall`/`FunctionCallOutput`**; all provider/native/custom/freeform tool items (and unknown variants) uniformly **hard fail** and return `ApiError`, never "drop with a warning", and never forcibly translate into a function call (that would make the model reference a tool the upstream doesn't have). Only codex's purely internal bookkeeping items may be dropped on egress, and this must be verified during implementation.

### 4.1 responses (passthrough = does not enter zmod routing)

Correction (eliminating the contradiction with §2.4 telemetry): **v1 has no responses connector inside zmod**. `connector = "responses"` (or a provider not listed in config-zmod) → `route()` returns **`None`** → goes directly to the **native branch** of `stream_responses_api` (`ApiResponsesClient` + full `.with_telemetry(...)`). This way the responses upstream has zero protocol translation while **fully preserving** codex-api request/SSE telemetry.

> Tradeoff: we originally wanted "to also apply the ① transform layer to the native Responses upstream", but that would require responses to enter zmod as well, thereby losing codex-api telemetry (§2.4). v1 chooses **telemetry first**: responses does not enter zmod. When we later want to do compression on responses passthrough, we will decide whether to pass telemetry into zmod, or insert a hook before the native branch that only modifies `ResponsesApiRequest` (without taking over the stream).

### 4.0b `tools` definition-level grading (Correction: inexpressible tools are intercepted at request-construction time)

`ResponsesApiRequest.tools` is `Vec<serde_json::Value>` (`codex-api/src/common.rs:188`), and may contain `function` / `namespace` / `tool_search` / `image_generation` / `web_search` / `custom` (freeform). The target protocol only supports some of these, so **we must grade by the tool definition's `type` at request-construction time** (corresponding to §4.0's history-item disposition, but earlier — to avoid sending an inexpressible tool definition in the first place):

| Tool definition `type` | chat | anthropic | Disposition |
|---|---|---|---|
| `function` (standard JSON schema) | `tools[{type:"function", function:{name,description,parameters}}]` | `tools[{name,description,input_schema}]` | **the only one supported in v1** |
| `custom` / freeform | **hard fail** | **hard fail** | no standard schema, cannot be expressed by a target function tool (same source as §4.0 CustomToolCall) |
| `namespace` | **hard fail** | **hard fail** | codex-proprietary namespace tool, not supported in v1 |
| `tool_search` / `web_search` / `image_generation` and other hosted/native tools | **hard fail** | **hard fail** | provider-side built-in tools, no equivalent definition in the target |

> That is: **v1 only admits standard `function` tool definitions**; if any other tool definition appears in the request → the connector returns an `ApiError` before constructing the egress request (matching §4.0's history-item hard fail, the two being consistent). This way we neither emit an inexpressible tool definition nor leave the inconsistency of "the tool definition wasn't sent, but the history contains a call to that tool".

### 4.0a endpoint path concatenation rule (Correction: eliminate version-segment ambiguity)

`egress_url = base_url.trim_end_matches('/') + path`, where `path` is **provided as a default by the connector and can be overridden by config-zmod's `path`**:

| connector | default `path` |
|---|---|
| chat | `/chat/completions` |
| anthropic | `/v1/messages` |
| responses | does not enter zmod (`route()` returns None, uses the native branch, §4.1) |

Convention: **`base_url` is written only up to the API root, not including the `path` segment above**; the version prefix (e.g. deepseek's `/v1`) counts as part of `base_url`. Therefore:

- deepseek `base_url = https://api.deepseek.com/v1` + default `/chat/completions` → `https://api.deepseek.com/v1/chat/completions` ✓
- anthropic official `base_url = https://api.anthropic.com` + default `/v1/messages` → `https://api.anthropic.com/v1/messages` ✓
- gateway-type (testkey) `base_url = https://node-hk.sssaiapi.com/api` + default `/v1/messages` → `https://node-hk.sssaiapi.com/api/v1/messages` (if the gateway path differs, override with `path`) ✓

### 4.2 chat (deepseek / OpenAI-compatible)

Egress `POST {egress_url}` (default `{base_url}/chat/completions`, see §4.0a), Bearer auth.

- **Request**: `instructions` → `messages[0]` system; `input[Message]` → `messages`; `input[FunctionCall]` → assistant `tool_calls` (`arguments` is already a JSON string, passed through directly); `input[FunctionCallOutput]` → `role:"tool"`; `tools` → `tools[{type:"function"}]`; `reasoning`/`store`/`include`/`prompt_cache_key` etc. graded per §7; add `stream:true` + `stream_options.include_usage`.
- **Response SSE → ResponseEvent**: `delta.content` → `OutputTextDelta` (for streaming display only) **and accumulate text**; `delta.tool_calls[].function.arguments` aggregated by index → `OutputItemDone(FunctionCall)`; **before completion, synthesize the assistant message's `OutputItemDone` per §4.5**; then emit `Completed{…}` (§4.5, `response_id` uses the chunk `id` or is synthesized, `end_turn` mapped from `finish_reason`); a top-level error → failure. `data:[DONE]` closes it out.

### 4.3 anthropic

Egress `POST {egress_url}` (default `{base_url}/v1/messages`, see §4.0a), headers `x-api-key` + `anthropic-version` (auth shaping in `http.rs`).

- **Request**: `instructions` → top-level `system`; `input[Message]` → `messages` (role only user/assistant); `input[FunctionCall]` → assistant `content[{type:"tool_use", id, name, input}]` (**`arguments` string → parse into an object**); `input[FunctionCallOutput]` → user `content[{type:"tool_result", tool_use_id, content}]`; `tools` → `tools[{name, description, input_schema}]`; **`max_tokens` required** — when absent, filled by config-zmod's `default_max_tokens` (fallback constant 4096).
- **Response SSE → ResponseEvent**: `content_block_delta`/`text_delta` → `OutputTextDelta` (display only) **and accumulate text**; `tool_use` block + `input_json_delta` aggregation (**object → stringify back into the `arguments` string**) → `OutputItemDone(FunctionCall)`; **before completion, synthesize the assistant message's `OutputItemDone` per §4.5**; `message_delta` (usage) + `message_stop` → `Completed{…}` (§4.5, `response_id` uses `message_start.message.id` or is synthesized, `end_turn` mapped from `stop_reason`); `error` → failure.
- **Hard constraint**: tool_use ↔ tool_result pairing must be complete — **when incomplete, repair per §4.10** (inject a synthetic result / delete orphan results / strip tool_choice when tools is empty), not hard fail; Reasoning egress disposition see §4.4.

### 4.5 Response completion contract (Correction: must synthesize the assistant message completion item + fully populate Completed)

`map_response_events` in `core/src/client.rs` **only collects `item` into `LastResponse.items_added` on `OutputItemDone(item)`** (`client.rs:1822`); `OutputTextDelta` is only responsible for streaming display and **does not enter history**. Chat/Anthropic do not have Responses' native `output_item.done(message)`, so **the connector must accumulate text itself and synthesize an assistant-message completion item before `Completed`**, otherwise the next round's history is missing the assistant reply:

```
OutputItemDone(ResponseItem::Message {
    role: "assistant",
    content: vec![ContentItem::OutputText { text: <all accumulated text> }],
    id / ... filled per protocol,
})
```

Send order: `OutputTextDelta…` (display) → each `OutputItemDone(FunctionCall)` (if there are tool calls) → **the synthesized `OutputItemDone(Message assistant)`** (if there is text) → `Completed`.

**All three `Completed` fields must be fully populated** (`codex-api/src/common.rs:88` `Completed { response_id, token_usage, end_turn }`):

| Field | chat | anthropic |
|---|---|---|
| `response_id` | the chunk top-level `id`, synthesized if absent (e.g. `llmswitch-<uuid>`) | `message_start.message.id`, synthesized if absent |
| `token_usage` | the last chunk's `usage` (`include_usage`) → `TokenUsage` | accumulated `usage` from `message_start` + `message_delta` → `TokenUsage` |
| `end_turn` | `finish_reason=="stop"` → `Some(true)`; `"tool_calls"` → `Some(false)`; `"length"`/unknown → `None` | `stop_reason=="end_turn"` → `Some(true)`; `"tool_use"` → `Some(false)`; `"max_tokens"`/unknown → `None` |

### 4.6 `FunctionCallOutput` content grading (Correction: cover structured/multimodal output)

The body of `FunctionCallOutputPayload` may be plain text, or `ContentItems(Vec<...>)`, which may contain `InputImage`, `EncryptedContent` (`protocol/src/models.rs`). The connector grades by content, and **must not** force-fit images into the "input image" rule, nor silently send encrypted content to the wrong place:

| Output content / field | chat | anthropic |
|---|---|---|
| `body` text / `ContentItems` plain text | `content` text of `role:"tool"` | `tool_result.content` text |
| `body` `InputImage` | **v1 hard fail** | **v1 hard fail** |
| `body` `EncryptedContent` | **hard fail** | **hard fail** |
| `success: Option<bool>` (tool success/failure status) | no equivalent field: when `success == Some(false)`, **prepend** a short failure marker to the `content` text (e.g. an `[tool error] ` prefix) + warn; `Some(true)`/`None` add nothing | mapped to `tool_result.is_error = (success == Some(false))` (natively supported by anthropic); `None` → not set |

> `success` handling rationale: Chat's `role:"tool"` has no success/failure field, and **silently losing the failure status misleads the model**, so prepend a text marker + warn; Anthropic has native `is_error`, so map directly.

> **Images always hard fail in v1 (Correction)**: neither config nor `ModelProviderInfo` has a `supports_images`/vision-capability field (verified), so the connector cannot determine whether the current DeepSeek/Claude model supports images. Therefore **v1 hard fails on all images (input images, tool image outputs)**, making no capability guesses. To support them in the future, first add an explicit capability field like `supports_images = true` to config-zmod before admitting them.
> Consistent with §4.4: encrypted content is only passed through on responses passthrough; the chat/anthropic egress hard fails on `EncryptedContent` (whether in a message, AgentMessage, or tool output) without exception.

### 4.4 The two kinds of reasoning objects (Correction: eliminate the conflict with §7.1)

**We must distinguish two things with the same name but different meanings**; their handling rules differ and do not conflict:

1. **`ResponseItem::Reasoning` in history** (the encrypted reasoning **output item** in `input[]`, containing `encrypted_content`): OpenAI-proprietary, unreadable by a non-Responses upstream, with no landing place for "passthrough". Disposition —
   - **chat / anthropic egress**: **not written into** the request body sent upstream (drop on egress). The connector only constructs a request **copy**; codex's local conversation history (the original `ResponseItem` list) **is unaffected**, and subsequent rounds still fully retain the reasoning.
   - **responses passthrough**: `encrypted_content` passed through verbatim (via the native client).
2. **The request-level `ResponsesApiRequest.reasoning: Option<Reasoning>`** (the reasoning **config**: effort / summary, **not encrypted, not history**, `codex-api/src/common.rs:191`): this is the subject of §7.1's "downgrade conversion" — anthropic → `thinking`, chat → `reasoning_effort`, dropped + warn if the target doesn't support it.

> In one sentence: **the encrypted reasoning output item (history) is dropped on egress; the reasoning config (request field) is downgraded per §7.1**. §4.4 and §7.1 each handle one of these, with no contradiction.

### 4.7 `run`'s error and spawn boundary (Correction: connection errors must return synchronously)

So that the outer `match stream_result` can correctly sort errors, `run` is internally split into two phases:

1. **Synchronous phase** (completed before `run` returns): construct the egress request → send HTTP → **validate the status code** → establish the SSE response reader. Any failure in this phase (connection failure, DNS, non-2xx status, auth 4xx) **directly `return Err(ApiError)`**, falling into the outer `match` (`map_api_error` + `inference_trace_attempt.record_failed`).
2. **Asynchronous phase** (`spawn`): **only when** phase 1 succeeds in obtaining a 2xx SSE does it `spawn` the reader task; stream errors within the task (SSE interruption, bad frames) go through `tx.send(Err(ApiError…))` → `map_response_events` stream error handling.

**401 note**: the outer `TransportError::Http{status==UNAUTHORIZED}` arm triggers the **OpenAI-specific** `handle_unauthorized` recovery, which is meaningless for a third-party upstream. To avoid a pointless recovery loop, the connector maps a third party's **401/403 and other auth failures to a plain `ApiError` (not the `UNAUTHORIZED` transport variant)**, so they fall into the generic failure arm and are reported directly.

### 4.8 Tool-call id mapping (Correction: fully populate the pairing fields)

`ResponseItem::FunctionCall.call_id: String` (`protocol/src/models.rs`, a stable id) is the pairing anchor. When constructing the request history on egress:

| Direction | chat | anthropic |
|---|---|---|
| FunctionCall (assistant call) | `messages[assistant].tool_calls[].id = call_id`, `function.name/arguments` filled too | `content[{type:"tool_use", id: call_id, name, input}]` |
| FunctionCallOutput (tool result) | `messages[tool].tool_call_id = call_id` | `content[{type:"tool_result", tool_use_id: call_id, content}]` |

Inbound (upstream SSE → `OutputItemDone(FunctionCall)`) in reverse: chat `tool_calls[].id` / anthropic `tool_use.id` → backfill into `call_id`, ensuring that in the next round's history the call ↔ output still pair by `call_id`. **The same `call_id` threads the assistant call and the tool result throughout**, with no separate id manufactured (unless the upstream gives no id at all, in which case the connector synthesizes one and keeps it consistent between call/result).

### 4.9 `Message.content` per-item mapping (Correction: the three ContentItem variants)

`ContentItem` has `InputText`/`InputImage`/`OutputText` (`protocol/src/models.rs`). All may appear in user/assistant history. Mapping:

| ContentItem | chat | anthropic |
|---|---|---|
| `InputText` | message `content` text (user/system) | message `content[{type:"text"}]` |
| `OutputText` | assistant message `content` text | assistant `content[{type:"text"}]` |
| `InputImage` | **v1 hard fail** (same as §4.6, no capability detection) | **v1 hard fail** |

> In v1 text (InputText/OutputText) maps normally, **images hard fail**; the role is determined by the enclosing `Message.role` (anthropic only user/assistant, system goes to the top-level `system`).

### 4.10 Tool pairing / config integrity (Correction: replicate llm-rosetta, no hard fail)

codex context compression can break request structure (orphaned tool_call/result, `tool_choice` present but no tools) — llm-rosetta's `fix_orphaned_tool_calls` / `strip_orphaned_tool_config` (`converters/anthropic/`, `converters/base/tools.py`) exist precisely for this. **This is exactly our scenario, and we should replicate its repair behavior, not hard fail** (hard failing would directly break a normal post-compression conversation):

- **Orphan tool_call / tool_use** (a call with no corresponding result) → **inject a synthetic placeholder result** (content like `[No output available yet]`), not hard fail.
- **Orphan tool_result / tool_output** (a result with no preceding call) → **delete that orphan result**, not hard fail.
- **`tool_choice`/`tool_config` present but `tools` empty** (compression deleted all tool definitions) → **strip the `tool_choice`/`tool_config`** + warn (otherwise the upstream reports "tool_choice is set but no tools are provided").
- **chat-specific: tool message reordering** (replicating llm-rosetta `_reorder_tool_messages`, `converters/openai_chat/message_ops.py`). Chat Completions requires that a `role:"tool"` message **immediately follow** the `role:"assistant"` message that produced the corresponding `tool_calls`; in the Responses format codex interleaves `function_call_output` with other items (see openai/codex PR #7038), so after converting to the flat chat message sequence the tool message gets separated from its assistant → upstream 400. **id pairing alone (§4.8) is not enough, reordering is mandatory**: before egress to chat, group tool messages by their respective `tool_call_id`, iterate over non-tool messages, and after each assistant carrying `tool_calls` insert back the matching tool messages **in `tool_calls` order**; unmatched tool messages are **appended to the end** (not silently dropped) + warn. Whenever reordering happens, log a warning.
  - **anthropic doesn't need this**: its `tool_result` is a content block within the user turn, grouped by turn (§4.3), not a flat message sequence, so there is no such flat-ordering problem.
- All of the above act only on the request **copy** sent upstream, not on codex's local history; each logs a warning.

> Note: this does not conflict with §4.0's "unsupported variant hard fail" — §4.0 handles **item types unsupported in v1** (native/custom tools), whereas §4.10 handles **structural damage to supported standard function tools caused by compression**; the latter is repaired, the former is rejected.

### 4.11 `tool_choice` / `parallel_tool_calls` mapping (Correction: unified rules)

The top-level tool-control fields of `ResponsesApiRequest` (not the tool definitions themselves):

- **`parallel_tool_calls`** (Correction: anthropic has a counterpart, must not be dropped):
  - chat → pass through `parallel_tool_calls`.
  - anthropic → mapped to `tool_choice.disable_parallel_tool_use` (llm-rosetta `anthropic/tool_ops.py`): codex `parallel_tool_calls == false` → `disable_parallel_tool_use = true`; `true`/unset → not set (anthropic defaults to parallel).
- **`tool_choice`** (Correction: unified as "a forced choice that changes tool-chain semantics and cannot be downgraded → hard fail"):
  - `auto` / `none` → mapped to the target's corresponding tier (chat `"auto"`/`"none"`; anthropic `{type:"auto"}` / `{type:"none"}`).
  - **Force-call a specific tool / force-must-call (`required` / a specified function)**: this changes tool-chain semantics. Map if the target can express it equivalently (chat `{type:"function", function:{name}}` / `"required"`; anthropic `{type:"tool", name}` / `{type:"any"}`); **when the target cannot equivalently express that forced semantics → hard fail** (no downgrade, no warn-and-admit — downgrading would make the model's behavior deviate from codex's expectation).
  - That is: **map if expressible, hard fail uniformly on any inexpressible forced tier** (replacing the earlier contradictory mix of "warn-and-admit" and "hard fail").

## 5. Configuration and Routing (config-zmod)

The routing key = codex's `model_provider_id` (§2.1). The codex-side `config.toml` configures the provider as usual, and llm-switch takes over before sending by matching the same id and rewriting protocol/path/auth. Each provider needs **two places** of configuration:

### 5.1 codex `~/.codex/config.toml` (both providers required)

```toml
# —— deepseek ——
[model_providers.deepseek]
name     = "DeepSeek"
base_url = "https://api.deepseek.com/v1"   # under takeover semantics this value does not participate in routing, the real upstream can be left here
wire_api = "responses"                     # codex only speaks Responses internally
env_key  = "DEEPSEEK_API_KEY"
supports_websockets = false

# —— claude ——
[model_providers.claude]
name     = "Claude"
base_url = "https://api.anthropic.com"
wire_api = "responses"
env_key  = "ANTHROPIC_API_KEY"
supports_websockets = false
```

To switch, set `model_provider = "deepseek"` (or `"claude"`).

### 5.2 codez `~/.codex/config-zmod.toml` (routing + egress translation)

```toml
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]          # table name = codex's model_provider_id
connector = "chat"
base_url  = "https://api.deepseek.com/v1"  # optional; defaults to the codex provider's base_url
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"             # the connector reads the raw key itself (see §5.3)
# path    = "/chat/completions"            # optional, overrides the default egress path (§4.0a)

[llm-switch.providers.claude]
connector         = "anthropic"
base_url          = "https://api.anthropic.com"
auth              = "x-api-key"
key_env           = "ANTHROPIC_API_KEY"
anthropic_version = "2023-06-01"
default_max_tokens = 8192
```

- If a table name matches, use the corresponding connector; if not → the native Responses path.
- If the file or `[llm-switch]` is missing → fully disabled (fail-safe, per codez's zmod convention).
- **`model` (optional)**: overrides/maps the model name sent to the real upstream. When taking over codex at runtime, it defaults to the `model` in codex's request; for standalone runs / live tests it is specified by this field (e.g. `deepseek-v4-pro`, `claude-opus-4-8` in testkey).

### 5.3 Key source and priority (Correction: the connector fetches the raw key itself, not relying on codex auth shaping)

**Key constraint**: codex's `api_auth` is `SharedAuthProvider = Arc<dyn AuthProvider>`, which only exposes `add_auth_headers()` (writing `Authorization: Bearer <token>`) and **does not expose the raw key** (`codex-api/src/auth.rs:68`, `model-provider/src/bearer_auth_provider.rs`). But Anthropic needs an `x-api-key` header — which **cannot** be reshaped from codex's auth. So the connector must **obtain the raw key itself**.

Key sources, by priority:

1. **`key_env`** (a per-provider field in config-zmod) → the connector reads the raw key directly via `std::env::var(key_env)`. **The main runtime-takeover path** (may point to the same environment variable as codex `config.toml`'s `env_key`, but is read independently by the connector, not through codex auth).
2. **inline `auth_key`** → **allowed only in the gitignored `zmod/llm-switch/tests/testkey.toml`**, for offline/live tests and standalone runs. **Definite policy (not the ambiguous "warn or reject")**: in a proper `~/.codex/config-zmod.toml`, **once `auth_key` appears, `config.rs` returns a config error during parsing and refuses to start** (a plaintext key must not land in codex's main config); the `auth_key` field is accepted only in the code path that loads from `tests/testkey.toml`.
3. **(fallback only when `auth = "bearer"` and neither `key_env`/`auth_key` is configured)** reuse codex's `api_auth.add_auth_headers()` to write `Authorization: Bearer` — because the bearer form matches codex, it can be leveraged directly. The x-api-key form has **no** such fallback and must have `key_env`/`auth_key`.

`http.rs` shapes the headers from the obtained raw key per `auth`: `bearer` → `Authorization: Bearer <key>`; `x-api-key` → `x-api-key: <key>` + `anthropic-version`. The `anthropic` connector validates key availability at startup, otherwise `ApiError` (fail early on missing config).

## 6. patch (all changes to codex-rs)

Correction: this is **not** a "3-spot checklist append + early return", but a real integration (because we need to route by id + join the existing stream_result flow). Touchpoints of `patches/llm-switch.patch`:

### 6.1 Build integration (Correction: not made a workspace member)

`zmod/llm-switch` is **outside** the codex-rs workspace root (`codex-rs/`), and needs to reverse-depend on `codex-api`/`codex-protocol` (which are members of codex-rs). Stuffing `../zmod/llm-switch` into codex-rs's `[workspace] members` is unsuitable (cross-root, and must sync `[workspace.dependencies]`). Instead:

- `zmod/llm-switch/Cargo.toml` is a **standalone package** (**does not declare its own `[workspace]`**, otherwise compiling it as a path dependency triggers a "nested workspace" error), pointing back via explicit path dependencies: `codex-api = { path = "../../codex-rs/codex-api" }`, `codex-protocol = { path = "../../codex-rs/protocol" }` (versions follow codex-rs, without `workspace = true`).
- the patch adds a path dependency in **`codex-rs/core/Cargo.toml`**: `codez-llm-switch = { path = "../../zmod/llm-switch" }`. It is compiled together as an ordinary path dependency of core, **not entering** the workspace member list; no dependency cycle (llm-switch only depends on api/protocol, not on core).
- standalone `cargo test`: run directly in `zmod/llm-switch/`; the path dependencies locate codex-rs's crates and resolve normally.

### 6.4 Build-convention documentation update (a deliverable, not optional)

Case B conflicts with the original `CLAUDE.md` convention "the patch adds `codez-<feature>` to workspace members". This is a **change to the repository's build convention**, and must be one of this feature's deliverables: update `CLAUDE.md`'s "zmod crate and patch naming rules / build integration" to make the two cases explicit (A: a standalone crate enters members; B: one reverse-depending on a codex-rs crate uses an external path dependency). Otherwise the implementation would violate the repository's existing convention.
> (Already landed in commit `7a12f5291`; registered here as a formal deliverable, included in the success criteria.)

### 6.2 Routing-key passthrough (Correction: ModelClient needs the id)

- **`core/src/client.rs`** `ModelClient::new` adds a parameter `model_provider_id: String`, stored into `ModelClientState.model_provider_id`; its sole caller passes it in from `Config.model_provider_id` (`config/mod.rs:631`).

### 6.3 Send-boundary integration (Correction: join stream_result, not early return)

- **`core/src/client.rs`** `stream_responses_api`: change the existing "construct `ApiResponsesClient` + `client.stream_request(...)`" section into a two-way choice based on `codez_llm_switch::route(&self.client.state.model_provider_id)` (see the §2 code). **Owned args**: `run`'s signature is `run(rt: Route, request: ResponsesApiRequest, api_provider: codex_api::Provider, api_auth: SharedAuthProvider, transport: ReqwestTransport, options: …) -> Result<codex_api::ResponseStream, ApiError>` — symmetric with the native arm moving `api_provider`/`api_auth`/`transport` into `ApiResponsesClient::new`; the two arms are mutually exclusive, the moves are legal, and **no borrow is retained**. **The downstream `match stream_result { … }` (unauthorized recovery / `map_api_error` / `record_failed` / `map_response_stream`) is entirely unchanged** and shared by both.

The translation/network logic lives entirely in the `codez-llm-switch` crate; the core touchpoints are a Cargo dependency + a `ModelClient::new` parameter + one assignment rewrite in `stream_responses_api`. The downstream error/`inference_trace`/`map_response_stream` logic is untouched; but **the takeover path does not wire up codex-api-layer request/SSE telemetry** (§2.4, a known gap), while the native path fully preserves `.with_telemetry(...)`. The conflict surface when syncing codex-rs is small but **non-zero** (`ModelClient::new`'s signature is a relatively stable interface).

## 7. Error Handling and Field Grading

- Connector translation/network failure → mapped to codex's existing `ApiError` (`tx.send(Err(..))` or `run` returns `Err` directly), and codex handles it via the native error flow (retry/report).

### 7.1 Request field grading (Correction: not uniformly warn-and-drop)

Graded into three tiers by "whether dropping changes model-visible semantics", with each connector disposing accordingly (`ResponseItem` variants see §4.0):

| Tier | Field (examples) | Disposition |
|---|---|---|
| **Safe to ignore** (pure transport/cache metadata, no effect on model output) | `store`, `include`, `prompt_cache_key`, `service_tier`, `client_metadata` | silently drop |
| **Downgrade conversion** (the target has an approximate expression, map best-effort, log a warning when real semantics are lost) | **the request-level `reasoning` config** (`ResponsesApiRequest.reasoning`, i.e. effort/summary, **not** the encrypted `Reasoning` item in history — the latter is dropped on egress per §4.4) → anthropic→`thinking` / chat→`reasoning_effort`, dropped+warn if absent; `text.format` structured-output schema (chat→`response_format` json_schema; anthropic has none→downgrade to instructions or warn) | map best-effort + warn when necessary |
| **Special-cased rules** (see the relevant subsection, not generalized here) | `parallel_tool_calls`, `tool_choice` → §4.11; orphan tool_call/result, tool_choice with empty tools → §4.10 | see §4.10 / §4.11 |
| **Must hard fail** (silently dropping breaks model-visible semantics/the tool chain, and cannot be downgraded) | image/multimodal input (§4.9/§4.6, always in v1); unsupported `ResponseItem` variants carrying tool calls/results (those marked "hard fail" in §4.0); a forced `tool_choice` the target cannot equivalently express (§4.11) | return `ApiError`, don't send the request |

> During implementation: each connector implements the table above item by item, with golden tests covering both "downgrade" and "hard fail" assertions.

### 7.2 Auth shaping

`http.rs`: `auth = "bearer"` → `Authorization: Bearer <key>`; `auth = "x-api-key"` → `x-api-key: <key>` + `anthropic-version`. **The raw key is fetched by the connector itself** (`key_env` / testkey's `auth_key`), not relying on codex's `add_auth_headers` (which can only produce Bearer). Source and priority see §5.3.

## 8. Tests and Success Criteria

- **Offline golden tests** (the mainstay, no key needed). Assert that `ResponsesApiRequest → target request JSON` is semantically equivalent (ignoring field order and optional-omission policy); a static SSE chunk sequence drives the connector, asserting the produced `ResponseEvent` sequence is correct (including tool_call aggregation, usage, finalization). **The baseline source for each connector must be explicit** (Correction: `rust-llm-proxy` only implements OpenAiChat, with no Rust baseline for Anthropic):
  - **chat**: can use `../3rd/proxy/rust-llm-proxy`'s OpenAiChat converter/fixtures as the baseline.
  - **anthropic**: use **`../3rd/proxy/llm-rosetta`'s Python anthropic converter** (`tests/converters/anthropic`) to generate expected output, frozen into this repo's fixtures; or declare a **self-built fixture** (manually verified against the official Anthropic Messages format). Choose one of the two; **do not** vaguely write "the corresponding converter".
  - Cover assertions for §7.1 downgrade / hard fail, §4.0 variant hard fail, and §4.0b tool-definition hard fail.
- **Integration/live tests** (gated): read `zmod/llm-switch/tests/testkey.toml` (schema is `[llm-switch.providers.<id>]` + `auth_key` + `model`), really hit the deepseek (chat)/claude (anthropic) endpoints, and verify end-to-end connectivity. Gate with `#[ignore]` or an environment variable; when `testkey.toml` is absent, automatically skip, so CI without a key still goes fully green, and locally `cargo test -- --ignored` runs the real chain.
- **Invariants**: chat/anthropic drop `Reasoning` on egress but codex's local history is unchanged (§4.4); on responses passthrough `encrypted_content` is unchanged; the `call_id` association of tool_call ↔ output is correct; variants marked "hard fail" in §4.0 do return `ApiError` rather than silently drop.
- **Standalone testability**: the crate can run `cargo test` standalone, detached from codex (path-depending on codex-api / codex-protocol).

Success criteria:

1. After codex is configured with `[model_providers.deepseek]` **and** `[model_providers.claude]` per §5.1 + config-zmod routing per §5.2, **both** can run a conversation. **Tool-call acceptance scope (eliminating the ambiguity with "v1 supports only function")**: live scenarios **enable only standard `function` tools** — i.e. verify only `FunctionCall`/`FunctionCallOutput` round-trips; the tests/config must **disable** the namespace/custom/tool_search/web_search/image_generation and other tools codex may expose by default (otherwise per §4.0/§4.0b they hard fail and cannot be accepted). The "hard fail on encounter" behavior of these disabled tools is covered by the offline golden tests (criterion 2), and is not part of the live criteria.
2. The offline golden tests for all three connectors are fully green (semantic equivalence), and cover both the §7.1 downgrade/hard-fail assertions.
3. The above hard invariants are satisfied.
4. The core touchpoints are only those listed in §6 (Cargo dependency + `ModelClient::new` parameter + one assignment rewrite in `stream_responses_api`), without modifying existing error/`inference_trace`/stream-mapping logic; the native path preserves `.with_telemetry(...)`, the takeover path does not wire up codex-api request/SSE telemetry (§2.4, recorded as a known gap).
5. `CLAUDE.md`'s zmod build convention has been updated to the case A/B split (§6.4), consistent with this feature's case-B integration approach.

## 9. Security Notes

`zmod/llm-switch/tests/testkey.toml` contains real API keys and is already excluded by `.gitignore` (line 30's global `testkey.toml` match); it must not be committed to GitHub. Any new test fixture containing keys must likewise be ensured to be covered by gitignore.
