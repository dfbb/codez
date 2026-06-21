# llm-switch Implementation Plan — Master Index

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to execute task by task. Each task is a standalone plan file, with steps tracked via `- [ ]` checkboxes.

**Goal:** Implement codez's first zmod `codez-llm-switch`, which takes over the LLM API layer inside the codex process: translate the Responses protocol that codex always emits into Anthropic Messages / Chat Completions toward the real upstream, and translate the upstream SSE back into `ResponseEvent`, so that codex can connect to non-OpenAI models such as deepseek/claude.

**Architecture:** An in-process two-layer pipeline hooked into `stream_responses_api` in `core/src/client.rs`: ① `TransformPlugin[]` transforms (passthrough in v1, compression hooked later) → ② `Connector` egress translation + HTTP/SSE. The connector consumes codex-native types (`codex-api` / `codex-protocol`) and returns a `codex_api::ResponseStream` of the same shape as `ApiResponsesClient::stream_request`, handed off to core's existing `map_response_stream` wrapper. The intrusion into codex-rs is expressed via `patches/llm-switch.patch`, without modifying the source directly.

**Tech Stack:** Rust 1.95.0, tokio, reqwest (SSE), serde / serde_json, `codex-api` / `codex-protocol` (path dependencies).

**Design basis:** `docs/superpowers/specs/2026-06-20-llm-switch-design.md` (finalized, commit `eee639773`). Each task in this plan is annotated with the spec section it covers.

## Global Constraints

Copied verbatim from the spec; the requirements of this section are implicitly included in every task:

- **Crate naming:** package name `codez-llm-switch`, directory `zmod/llm-switch/`, lib target `codez_llm_switch` (spec §1).
- **Do not declare its own `[workspace]`:** otherwise compiling it as a path dependency triggers a nested-workspace error (spec §6.1).
- **Reverse path dependencies:** `codex-api = { path = "../../codex-rs/codex-api" }`, `codex-protocol = { path = "../../codex-rs/protocol" }`, **not** `workspace = true` (spec §6.1).
- **Not added to workspace members:** the patch adds `codez-llm-switch = { path = "../../zmod/llm-switch" }` in `codex-rs/core/Cargo.toml` (spec §6.1).
- **Routing key = `model_provider_id`:** must not use `name` or `base_url` (spec §2.1).
- **Return `codex_api::ResponseStream`:** its fields `rx_event` / `upstream_request_id` are `pub`, constructed via `mpsc::channel`; same shape as `ApiResponsesClient::stream_request` (spec §2.2, verified against the source).
- **Error/spawn boundary:** `run` synchronously completes HTTP + status-code validation + SSE establishment; non-2xx directly `return Err(ApiError)`; only on 2xx does it `spawn` the reader task (spec §4.7).
- **Map third-party 401/403 to a plain `ApiError`** (not `TransportError::Http{status==UNAUTHORIZED}`), to avoid triggering OpenAI-specific recovery (spec §4.7).
- **v1 tool capability supports only standard `function`:** any provider/native/custom/freeform tool items and unknown variants → hard-fail returning `ApiError`, never silently dropped, never force-translated into functions (spec §4.0 / §4.0b).
- **Images hard-fail unconditionally in v1:** no capability-detection field, no guessing (spec §4.6 / §4.9).
- **Encrypted content** (`EncryptedContent` / `Compaction` / `ContextCompaction`) → hard-fail; `Reasoning` history items → dropped outbound; `CompactionTrigger` → dropped outbound (spec §4.0 / §4.4).
- **The connector only constructs a request copy,** never mutating codex's local history (spec §4.0 / §4.4 / §4.10).
- **Keys:** the connector fetches the raw key itself (`key_env` / testkey's `auth_key`), without relying on codex `add_auth_headers` (which can only produce Bearer); if `auth_key` appears in a production config-zmod → reject startup with a config error at parse time (spec §5.3).
- **fail-safe:** missing config file or missing `[llm-switch]` → fully disabled (spec §5.2).
- **Security:** `tests/testkey.toml` contains a real key, already covered by `.gitignore` line 30, and must not be committed (spec §9).
- **Rust style:** non-test code avoids `unwrap`/`expect`; TUI color rules do not apply to this crate.

## Real types nailed down at the implementation layer (avoid guessing from memory)

- `ResponsesApiRequest`: `model: String`, `instructions: String`, `input: Vec<ResponseItem>`, `tools: Vec<serde_json::Value>`, `tool_choice: String`, `parallel_tool_calls: bool`, `reasoning: Option<Reasoning>`, `store/stream: bool`, `include: Vec<String>`, `service_tier/prompt_cache_key: Option<String>`, `text: Option<TextControls>`, `client_metadata: Option<HashMap<String,String>>` (`codex-api/src/common.rs:182`).
- `ResponseEvent`: `OutputTextDelta(String)`, `OutputItemDone(ResponseItem)`, `Completed{response_id:String, token_usage:Option<TokenUsage>, end_turn:Option<bool>}`, `ToolCallInputDelta{item_id,call_id:Option<String>,delta}`, etc. (`codex-api/src/common.rs:73`).
- `ResponseStream { pub rx_event: mpsc::Receiver<Result<ResponseEvent, ApiError>>, pub upstream_request_id: Option<String> }` (`codex-api/src/common.rs:305`).
- `ResponseItem` (16 variants), `ContentItem { InputText{text} | InputImage{image_url,detail} | OutputText{text} }`, `AgentMessageInputContent { InputText{text} | EncryptedContent{encrypted_content} }`, `FunctionCall{ id, name, namespace:Option<String>, arguments:String, call_id:String, .. }`, `FunctionCallOutputPayload{ body:FunctionCallOutputBody, success:Option<bool> }`, `FunctionCallOutputBody { Text(String) | ContentItems(Vec<FunctionCallOutputContentItem>) }` (`protocol/src/models.rs`).
- `TokenUsage{ input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens, total_tokens: i64 }` (`protocol/src/protocol.rs:2000`).
- `ApiError` (`codex-api/src/error.rs:8`), `TransportError::Http{status:StatusCode, url, headers, body}` (`codex-client/src/error.rs:6`).
- `SharedAuthProvider = Arc<dyn AuthProvider>`, `AuthProvider::add_auth_headers(&self, &mut HeaderMap)` (`codex-api/src/auth.rs`).
- core integration points: `ApiResponsesClient::new(transport, api_provider, api_auth).with_telemetry(...).stream_request(request, options)` (`core/src/client.rs:1324`); `map_response_stream(api_stream, session_telemetry, inference_trace_attempt)` (`client.rs:1758`).

## Dev-time build and test (architecture decision, set 2026-06-20; see CLAUDE.md, case B "Dev-time testing")

`zmod/llm-switch` reverse-depends on codex-api/codex-protocol (whose dependencies are all `{ workspace = true }`, with versions pinned by the codex-rs workspace). Two cargo hard constraints have been confirmed by testing: ① as a "non-member path dependency" it **cannot use `[dev-dependencies]` and cannot run `tests/*.rs` integration tests**; ② cargo **rejects members outside of codex-rs**. Yet Task 08 needs `wiremock` (dev-dep), and several tasks use `tests/*.rs` golden tests.

**Decision: during development (Task 01–08), use a symlink to bring this crate into the codex-rs workspace as a real member**, thereby fully supporting dev-deps + integration tests, while sharing codex-rs's `Cargo.lock`/`target` (no drift, reusing the already-compiled codex-api tree). Implementation:

- **Symlink + members (dev-only scaffolding):**
  ```bash
  ln -s ../zmod/llm-switch codex-rs/llm-switch    # already created; cargo treats it as a member under the root
  # add a line at the end of members in codex-rs/Cargo.toml: "llm-switch",
  ```
  The symlink `codex-rs/llm-switch` is added to the root `.gitignore` (`/codex-rs/llm-switch`); the members line and the build-generated `codex-rs/Cargo.lock` change are kept uncommitted dirty. **None of these go into `patches/llm-switch.patch` or get committed into the codex-rs subtree.**
- **Unified test command:** in each task brief, `cd zmod/llm-switch && cargo test --test X` should always be read as **`cd codex-rs && cargo test -p codez-llm-switch`** (or `--test X`). Integration tests and dev-deps are fully usable thanks to member status.
- **The crate itself:** in `zmod/llm-switch/Cargo.toml`, codex-api/codex-protocol are **active** path dependencies (not commented out); other versions align with the workspace; it does not declare its own `[workspace]`; `[dev-dependencies]` are declared normally (usable under member status); it does not commit its own `Cargo.lock`.
- **Production integration unchanged:** the Task 09 patch is still a core path dependency + a client.rs call site (case B); the symlink/members are only for dev testing and are unrelated to the patch. `git reset --hard` undoes the members line (the symlink survives because it is ignored); rebuild it with the two commands above.

## Task dependency graph

```
01 crate-skeleton-config ─┬─> 02 http-auth ──────────┐
                          ├─> 03 pipeline-connector ──┼─> 04 chat-request ─> 05 chat-sse ─┐
                          │                           ├─> 06 anthr-request ─> 07 anthr-sse ┼─> 08 run-sse-reader ─> 09 patch-core ─> 10 live-tests
                          └───────────────────────────┘                                   ┘
```

Suggested execution order: 01 → 02 → 03 → (04→05 and 06→07 can run in parallel) → 08 → 09 → 10.

## Task list

1. [Task 01 — crate skeleton and config](2026-06-20-llm-switch-01-crate-skeleton-config.md)
2. [Task 02 — http.rs egress and auth](2026-06-20-llm-switch-02-http-auth.md)
3. [Task 03 — pipeline and connector trait/factory](2026-06-20-llm-switch-03-pipeline-connector-trait.md)
4. [Task 04 — chat outbound request construction](2026-06-20-llm-switch-04-chat-request.md)
5. [Task 05 — chat SSE→ResponseEvent](2026-06-20-llm-switch-05-chat-sse.md)
6. [Task 06 — anthropic outbound request construction](2026-06-20-llm-switch-06-anthropic-request.md)
7. [Task 07 — anthropic SSE→ResponseEvent](2026-06-20-llm-switch-07-anthropic-sse.md)
8. [Task 08 — run() wiring and SSE reader](2026-06-20-llm-switch-08-run-sse-reader.md)
9. [Task 09 — patch integration into codex-rs core](2026-06-20-llm-switch-09-patch-core.md)
10. [Task 10 — testkey-gated live tests](2026-06-20-llm-switch-10-live-tests.md)

## Success criteria (after the whole plan is complete, against spec §8)

1. After codex is configured per §5.1 + §5.2, **both** deepseek (chat) and claude (anthropic) can complete a conversation (only standard `function` tools enabled).
2. All three connectors (including responses going through the native branch) pass their offline golden tests green, covering §7.1 downgrade/hard-fail and §4.0/§4.0b variant hard-fail assertions.
3. Hard invariants hold: `Reasoning` is dropped outbound but local history is unchanged; `call_id` pairing is correct; variants marked "hard-fail" do return `ApiError`.
4. core touchpoints are only those listed in §6 (Cargo dependency + `ModelClient::new` parameter + a single rewrite in `stream_responses_api`); the native path retains `.with_telemetry(...)`, the takeover path does not wire codex-api request/SSE telemetry (recorded as a known gap).
5. The zmod build convention in `CLAUDE.md` is already split into cases A/B (landed in `7a12f5291`).
