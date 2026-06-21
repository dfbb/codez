# llm-compress Implementation Plan — Master Index

> **For agentic workers:** REQUIRED SUB-SKILL: execute task by task using superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Each task is a standalone plan file; track steps with `- [ ]` checkboxes.

**Goal:** Implement codez's second zmod `codez-llm-compress`. At codex's LLM request send boundary (`stream_responses_api` in `core/src/client.rs`), perform in-process, irreversible but conservative compression on the already-assembled `ResponsesApiRequest` to reduce the token volume sent upstream, and record every effective compression in a CSV stats log.

**Architecture:** A single entry point `transform(&mut request, &api_provider, queryid)`, inserted once after `prepare_response_items_for_request` and before `record_started`, **compress first, then route**, fully transparent to downstream (native `stream_request` / future llm-switch / SSE parsing). Internally a two-layer pipeline: Layer 0 toggle & budget gate → Layer 1 walk `Vec<ResponseItem>` to extract tool-output text → Layer 2 ContentRouter picks a compressor by content recognition → Layer 3 the compressor's internal pipeline (text type: head/tail truncation + bare placeholder; structured type: in-JSON compression + parse validation). The intrusion into codex-rs is expressed via `patches/llm-compress.patch`, never by editing the source directly.

**Tech Stack:** Rust (edition 2021), serde / serde_json, toml, chrono (log timestamps), tracing; dev-dep `insta` (snapshots). Reverse path dependencies on `codex-api` / `codex-protocol`.

**Design basis:** `docs/superpowers/specs/2026-06-20-llm-compress-design.md` (finalized and cross-checked against the source along three dimensions, commit `58bbbf4ff`). Each task in this plan notes the spec sections it covers.

---

## Global Constraints

Copied verbatim from the spec; every task's requirements implicitly include this section:

- **crate naming**: package name `codez-llm-compress`, directory `zmod/llm-compress/`, lib target `codez_llm_compress` (spec §11).
- **Do not declare its own `[workspace]`**: otherwise compiling it as a path dependency triggers a nested-workspace error.
- **Reverse path dependencies**: `codex-api = { path = "../../codex-rs/codex-api" }`, `codex-protocol = { path = "../../codex-rs/protocol" }`, **not** `workspace = true`.
- **Production wiring does not enter workspace members**: zmod lives outside the codex-rs root tree; the **production** patch wires it in by adding the external path dependency `codez-llm-compress = { path = "../../zmod/llm-compress" }` in `codex-rs/core/Cargo.toml`, without entering members (spec §11). The **dev period** additionally uses a symlinked member (see "Dev-period build and test" below); the two coexist without affecting each other.
- **Single-point integration intrusion**: insert just two lines (queryid binding + `transform` call) in `core/src/client.rs` `stream_responses_api`, after `prepare_response_items_for_request` and before `record_started`; do not change any codex function signatures (spec §2).
- **transform signature nailed down**: `pub fn transform(request: &mut ResponsesApiRequest, api_provider: &ApiProvider, queryid: &str)`, returns `()`. The second parameter is a short read-only borrow used only for discrimination; **do not** clone/hold/return that reference; returning `()` rather than `Result` makes it type-impossible for a compression failure to block the request (spec §1/§2/§8).
- **queryid = `responses_metadata.thread_id`**: corresponds exactly to the rollout file name UUID; **do not** use `session_id` (a child agent inherits the parent's, so it cannot locate the specific rollout file) (spec §3).
- **Only two ResponseItem variants are processed**: `FunctionCallOutput` and `CustomToolCallOutput` (both have `output: FunctionCallOutputPayload`); all other variants are left untouched. MCP output, via `impl From<ResponseInputItem> for ResponseItem`, has already been converted to `FunctionCallOutput`, so it falls within scope (spec §4).
- **Text extraction rules**: `FunctionCallOutputBody::Text(s)` → compress `s`; `FunctionCallOutputBody::ContentItems(items)` → **per item, compress only** the `text` of `InputText{text}`; `InputImage`/`EncryptedContent` are neither read nor changed; **never** flatten ContentItems (spec §4).
- **fail-open throughout**: any compressor `compress()` panic → caught by `catch_unwind` → that fragment passes through verbatim; config parse failure → treated as `enabled=false` + warn; post-compression JSON parse failure → discard the compression, fall back to the original; log write failure → warn, request unaffected (spec §6/§8).
- **Irreversible + conservative thresholds**: compress only oversized items beyond the threshold; placeholder markers explicitly disclose the omission — text type uses a bare marker `[llm-compress: …]`, structured (JSON) type carries it via a valid JSON value (spec §6).
- **JSON does not go through the text-level flow**: JsonCompressor must compress within the JSON structure, express placeholders as JSON values, and the product must be re-parsed and validated by `serde_json`, falling back to the original on failure; **do not** apply text-level head/tail truncation or insert bare markers on JSON (spec §4/§6).
- **Stats log**: `~/.codex/log/llm-compress.log`, append; only when the overall `saved_bytes>0` write one headerless four-column CSV line: `timestamp (RFC3339 UTC), queryid, bytes before compression, bytes after compression`; size convention = total text bytes of the input items (spec §7).
- **Config fail-safe**: `~/.codex/config-zmod.toml` with no `[llm_compress]` section or `enabled=false` → fully off, zero-change path (spec §5).
- **Rust style**: non-test code avoids `unwrap`/`expect` (except at the panic boundary inside catch_unwind).

---

## Real types nailed down at the implementation layer (don't guess from memory; all verified against the source)

- Integration point: `core/src/client.rs:1309` `let mut request = self.client.build_responses_request(...)?;`; 1318-1320 `let store = request.store; self.client.prepare_response_items_for_request(&mut request.input, store);`; 1321 `inference_trace.start_attempt()`; 1323 `record_started(&request)`; 1324-1330 `ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth).with_telemetry(...).stream_request(request, options)`.
- `ResponsesApiRequest.input: Vec<ResponseItem>` (corroborated by `prepare_response_items_for_request(&mut [ResponseItem], bool)`).
- `enum ResponseItem` (`protocol/src/models.rs`) 16 variants: `Message, AgentMessage, Reasoning, LocalShellCall, FunctionCall, ToolSearchCall, FunctionCallOutput, CustomToolCall, CustomToolCallOutput, ToolSearchOutput, WebSearchCall, ImageGenerationCall, Compaction, CompactionTrigger, ContextCompaction, Other` — **excludes** `McpToolCallOutput` (which belongs to `ResponseInputItem`, models.rs:820).
- `FunctionCallOutput { call_id: String, output: FunctionCallOutputPayload }` (around models.rs:1010); `CustomToolCallOutput { call_id: String, name: Option<String>, output: FunctionCallOutputPayload }` (around models.rs:1042).
- `FunctionCallOutputPayload { body: FunctionCallOutputBody, success: Option<bool> }` (models.rs:1778); `enum FunctionCallOutputBody { Text(String), ContentItems(Vec<FunctionCallOutputContentItem>) }` (models.rs:1785); `enum FunctionCallOutputContentItem { InputText{text:String}, InputImage{image_url:String, detail:Option<ImageDetail>}, EncryptedContent{encrypted_content:String} }` (models.rs:1705).
- `impl From<ResponseInputItem> for ResponseItem` (models.rs:1486), the `McpToolCallOutput` branch (1506) converts to `ResponseItem::FunctionCallOutput` (1508).
- `CodexResponsesMetadata` (`core/src/responses_metadata.rs:135`) contains `session_id: String` (137) and `thread_id: String` (138), both `pub(crate)`; the `stream_responses_api` parameter `responses_metadata: &CodexResponsesMetadata` is reachable in scope.
- `ApiProvider` = `codex_api::Provider` (`core/src/client.rs:23` `use codex_api::Provider as ApiProvider;`), `#[derive(Clone)]` non-Copy.
- Borrow verification: `&responses_metadata.thread_id` yields `&String`, passing it as `queryid: &str` works via Deref coercion automatically — **no** `.as_str()` needed; `&client_setup.api_provider` is a short borrow dropped at the end of the transform call statement, after which `api_provider`/`api_auth` can each be moved by value.

---

## Dev-period build and test (following the llm-switch decision, 2026-06-20)

`zmod/llm-compress` reverse-depends on codex-api/codex-protocol (case B, CLAUDE.md §44-63). cargo hard constraints (empirically verified by llm-switch): **a non-member path dependency cannot declare `[dev-dependencies]` and cannot run `tests/*.rs` integration tests** (`cargo test -p` reports `requires dev-dependencies and is not a member`); and cargo also rejects a member outside the codex-rs root. **Decision: during dev (Task 01–08) use a symlink to wire the crate into the codex-rs workspace as a real member, building/testing inside the workspace.**

Implementation (following llm-switch, CLAUDE.md §54-63):

- **Symlinked member**: `ln -s ../zmod/llm-compress codex-rs/llm-compress` (cargo treats it as a member under the root, bypassing the cross-root restriction); append `"llm-compress",` (the symlink name, not a `../` path) to the end of `members` in `codex-rs/Cargo.toml`; add `/codex-rs/llm-compress` to the root `.gitignore`. Once the symlink is in place the crate is a formal member, fully supporting `[dev-dependencies]` and `tests/*.rs`, and sharing codex-rs's `Cargo.lock` and `target` (no version drift, reuses the already-compiled codex-api, fast).
- **Unified test command**: `cd codex-rs && cargo test -p codez-llm-compress ...`.
- **Dev-only scaffolding deliberately dirty/untracked**: the symlink `codex-rs/llm-compress` (gitignored), the members line in `codex-rs/Cargo.toml`, and the build-generated `codex-rs/Cargo.lock` all stay uncommitted throughout; **do not** commit them into the codex-rs subtree, **do not** put them in `patches/llm-compress.patch`, and **do not** revert them. Each task commits only `zmod/llm-compress/**` (and codez's own plans/patches).
- **Production wiring is independent of the symlink**: Task 09's patch takes the other path of case B — add the external path dependency `codez-llm-compress = { path = "../../zmod/llm-compress" }` in `codex-rs/core/Cargo.toml` + the client.rs call, exported into `patches/llm-compress.patch`.
- **The crate itself**: `zmod/llm-compress/Cargo.toml`'s codex-api/codex-protocol are activated path dependencies; the rest of the versions align with the workspace; it does not declare its own `[workspace]`; it does not commit its own `Cargo.lock` (gitignored).

---

## Task dependency graph

```
01 crate-skeleton-config ─┬─> 02 router-trait ─┬─> 03 truncate ─┐
                          │                    ├─> 04 json ─────┤
                          │                    ├─> 05 diff ─────┼─> 08 transform-entry ─> 09 patch-core
                          │                    └─> 06 log ──────┤
                          └─> 07 stats-log ────────────────────┘
```

- **01** is the foundation (crate + config); all tasks depend on it.
- **02** defines the `Compressor` trait / `Budget` / `ContentRouter` (including fail-open); it is the contract for 03–06.
- **03–06** the four compressors are independent of each other and can run in parallel; all depend on 02's trait.
- **07** the stats CSV log, standalone, depends only on 01's crate.
- **08** the transform entry, orchestrating Layer 0-3 + ResponseItem walk + text extraction + calling the router + calling stats, depends on 02–07.
- **09** patch core (Cargo.toml dependency + the two client.rs lines) + export the patch + restore the codex-rs working tree + live verification, depends on 08.

---

## Task list

| # | File | Deliverable | spec |
|---|------|--------|------|
| 01 | `2026-06-20-llm-compress-01-crate-skeleton-config.md` | crate skeleton + `config.rs` reading `[llm_compress]` | §5/§11 |
| 02 | `2026-06-20-llm-compress-02-router-trait.md` | `Compressor` trait + `Budget` + `ContentRouter` (fail-open) | §4 |
| 03 | `2026-06-20-llm-compress-03-truncate.md` | `TruncateCompressor` (fallback) | §6 |
| 04 | `2026-06-20-llm-compress-04-json.md` | `JsonCompressor` (in-structure compression + parse validation) | §6 |
| 05 | `2026-06-20-llm-compress-05-diff.md` | `DiffCompressor` | §6 |
| 06 | `2026-06-20-llm-compress-06-log.md` | `LogCompressor` | §6 |
| 07 | `2026-06-20-llm-compress-07-stats-log.md` | `stats.rs` CSV stats log | §7 |
| 08 | `2026-06-20-llm-compress-08-transform-entry.md` | `lib.rs` `transform()` orchestration + ResponseItem walk/extraction | §1/§2/§4/§8 |
| 09 | `2026-06-20-llm-compress-09-patch-core.md` | patch core + export patch + live verification | §2/§11 |
