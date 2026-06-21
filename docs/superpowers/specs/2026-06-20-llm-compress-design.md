# llm-compress zmod Design Document

**Date**: 2026-06-20
**crate**: `codez-llm-compress` (`zmod/llm-compress/`)
**Goal**: At codex's LLM request-sending boundary, perform in-process compression on the already-assembled `ResponsesApiRequest` to reduce the token volume sent upstream. It draws on headroom's content routing (content_router) and rtk's staged filtering pipeline (TOML DSL with 8 stages).

---

## 1. Positioning and Boundaries

- **What it is**: A standalone Rust crate that provides a single entry point `transform(request, provider, queryid) -> ResponsesApiRequest`, applying irreversible but conservative compression to the request content before it is sent upstream.
- **What it is not**: It does not swap the upstream (that's the job of its sibling zmod `llm-switch`), does not alter the response stream, does not do reversible retrieval (CCR), and does not do token counting (deferred to later iterations).
- **Relationship to llm-switch**: Both hook into the same codex integration point, but their **responsibilities are orthogonal**. llm-compress is a **standalone crate that intercepts up front**: compress first, then route. Compression applies to **all** request paths (including the native OpenAI responses path), independent of whether llm-switch matches a route.

---

## 2. Integration Point (single point of intrusion)

**File**: `codex-rs/core/src/client.rs`
**Function**: `stream_responses_api()` (around line 1270)
**Location**: After `build_responses_request(...)`, and before constructing `ApiResponsesClient` / entering the routing branch (currently around lines 1318-1330).

```rust
let mut request = self.client.build_responses_request(...)?;
let store = request.store;
self.client.prepare_response_items_for_request(&mut request.input, store);

// ── llm-compress front intercept (standalone zmod, independent of switch) ──
let queryid = &responses_metadata.thread_id;   // readily reachable in codex, see §3
let request = codez_llm_compress::transform(
    request,
    &client_setup.api_provider,
    queryid,
);

// ── existing routing branch (llm-switch or native), receives the already-compressed request ──
let stream_result = match codez_llm_switch::route(...) {
    None => {
        let client = ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
        client.stream_request(request, options).await
    }
    Some(rt) => codez_llm_switch::run(rt, request, ...).await,
};
```

**Key properties**:

- Both the input and output of `transform()` are codex's native `ResponsesApiRequest`, completely transparent to downstream (switch / native stream_request / SSE parsing / error handling).
- When disabled (config has no `[llm_compress]` or `enabled=false`), `transform()` returns the request unchanged, equivalent to a zero-modification path.
- The intrusion on the codex side is only two spots, encapsulated in `patches/llm-compress.patch`:
  1. Add the dependency `codez-llm-compress = { path = "../../zmod/llm-compress" }` to `core/Cargo.toml`.
  2. Add the two lines above in `client.rs`: the queryid retrieval + the `transform()` call.
- **No codex function signature is modified**: `queryid` is obtained from the in-scope existing parameter `responses_metadata.thread_id`.

---

## 3. Source of queryid

`queryid` = `responses_metadata.thread_id` (a `CodexResponsesMetadata` field, `pub(crate)`, reachable within the core crate).

> **Correction (consistent with the implementation/plan index)**: An early draft wrote `session_id`; this has been changed to `thread_id`. Reason: `session_id` is inherited by child agents from the parent and cannot pinpoint a specific rollout file; only `thread_id` (`ThreadId`, UUIDv7) corresponds exactly to the UUID in the rollout file name.

This value is codex's thread UUID, identical to the UUID in the rollout file name:

```
~/.codex/sessions/2026/05/18/rollout-2026-05-18T13-35-50-019e3995-5cd9-75a2-b487-f7959835f69e.jsonl
                                                          └────────────── thread_id ──────────────┘
```

Source chain: `CodexResponsesMetadata.thread_id` (`core/src/responses_metadata.rs`, populated from the session's `ThreadId.to_string()`) → passed as the parameter `responses_metadata: &CodexResponsesMetadata` into `stream_responses_api`, and the patch takes `responses_metadata.thread_id.clone()`.

Therefore the queryid in the compression log corresponds exactly to a specific rollout file.

---

## 4. Internal Two-Layer Pipeline

Inside `transform(request, provider, queryid) -> ResponsesApiRequest`:

```
ResponsesApiRequest
   │
   ▼
[Layer 0] Switch & budget gate
   • Read config [llm_compress].enabled —— off → return unchanged
   • Estimate the total text volume of the request input; below min_total_bytes → return unchanged (don't bother with small requests)
   ▼
[Layer 1] Iterate over each item in request.input[]
   For each InputItem (mainly function_call_output / large text items):
     • Take the text payload; below per_item_min_bytes → skip (conservative threshold)
     ▼
   [Layer 2] ContentRouter content detection (draws on headroom content_router)
     Detect in fixed priority order; the first match is responsible for compressing:
       ① JsonCompressor      (serde_json can parse it)
       ② DiffCompressor      (contains @@/diff --git/--- a/ headers)
       ③ LogCompressor       (multi-line + repeated lines/timestamps/stack-trace features)
       ④ TruncateCompressor  (detect always true, fallback)
     ▼
   [Layer 3] Compressor internal pipeline (draws on rtk TOML DSL stages)
     strip_ansi → (compressor-specific logic) → head/tail retention → max_bytes truncation
     → insert placeholder marker "[llm-compress: omitted N lines/bytes]"
   ▼
[Exit] If overall saved_bytes > 0 → write the stats log (§7); return the compressed request
```

### Core trait

```rust
/// Content detection + compression
trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;            // does it claim this content
    fn compress(&self, text: &str, budget: &Budget) -> CompressResult;
}

enum CompressResult {
    Compressed { text: String, saved_bytes: usize },
    Unchanged,                  // claimed it but judged not worth compressing
}
```

**ContentRouter**: Fixed priority `Json → Diff → Log → Truncate`, running `detect` in turn; the first match executes `compress`. Truncate is always the fallback, guaranteeing that any text over the threshold has a handler.

**fail-open**: If any compressor panics or throws during `compress()` (caught by `std::panic::catch_unwind`) → that item is **passed through verbatim**; a compression failure must never affect the request.

---

## 5. Configuration

File: `~/.codex/config-zmod.toml`, section `[llm_compress]` (same file as llm-switch, separate section). A missing section = `enabled=false` (fail-safe). Reading happens in `src/config.rs`, read once and cached in-process.

```toml
[llm_compress]
enabled = false                 # off by default
min_total_bytes = 4096          # skip the whole request when input text total is below this
per_item_min_bytes = 1024       # don't compress an item below this size (conservative threshold)

[llm_compress.truncate]
head_lines = 50
tail_lines = 50
max_bytes  = 16384              # per-item post-compression cap

[llm_compress.json]
max_array_items = 20            # array longer than this → sample head/tail + count
max_depth = 6                   # over-deep nesting → truncate to "…"

[llm_compress.diff]
context_lines = 3               # context lines retained per hunk

[llm_compress.log]
dedup_repeats = true            # collapse consecutive repeated lines into "(previous line ×N)"
```

---

## 6. Strategies of the Four Compressors

| Compressor | detect basis | compress strategy |
|--------|------------|--------------|
| **Json** | `serde_json::from_str` succeeds | Sample long arrays (keep head/tail + `"…(N more)"`); truncate over-deep nesting to `"…"`; preserve the rest of the structure. **The output must still be valid JSON** —— if re-parsing the compressed result fails, discard it and fall back to the original. |
| **Diff** | contains `@@ ... @@` / `diff --git` / `--- a/` lines | For each hunk, keep the changed lines + `context_lines` of context, drop excess context, and keep file headers. |
| **Log** | multi-line + timestamps / consecutive repeated lines / `at ...:line` stack features | Collapse consecutive repeated lines with a count; keep head/tail, collapse the middle into `[llm-compress: omitted N lines]`. |
| **Truncate** | always true (fallback) | strip ANSI → keep `head_lines` + `tail_lines`, replace the middle with `[llm-compress: omitted N lines / M bytes]`. |

The **placeholder marker** uses a unified format `[llm-compress: …]`, making it clear to the model that something has been omitted here (irreversible but explicit).

---

## 7. Compression Stats Log

**File**: `~/.codex/log/llm-compress.log` (create the directory if it doesn't exist; append mode).
**Trigger**: Append one line after a request is **effectively compressed** (overall `saved_bytes > 0`). Pass-through / no-match / disabled states are **not recorded**.
**Format**: CSV, four columns, no header:

```
timestamp,queryid,bytes_before,bytes_after
```

Example:

```
2026-06-20T08:15:30Z,019e3995-5cd9-75a2-b487-f7959835f69e,18432,5120
```

| Column | Source |
|----|------|
| timestamp | RFC3339 UTC (`chrono`) |
| queryid | `responses_metadata.thread_id` (the rollout file name UUID) |
| bytes_before | total text bytes of the input items at the transform entry |
| bytes_after | total text bytes of the input items at the transform exit |

**Size measure**: the sum of the text bytes of the input items (the actual target of the compressors), not the serialized bytes of the whole request.
**Implementation**: `log_compression(queryid, before, after)` in `src/stats.rs`, using `OpenOptions::append`.
**fail-open**: If writing the log fails (disk full / permissions), only record a single tracing warn; never affect the request.

---

## 8. Error Handling (fail-open throughout)

- The signature of `transform()` is `-> ResponsesApiRequest` (**does not return a Result**), structurally ruling out "a compression failure blocking the request".
- A panic inside a single compressor → caught by `catch_unwind` → that item is passed through verbatim.
- Config parse failure → treated as `enabled=false`, record a warn, take the zero-modification path.
- The compressed JSON must be re-parseable; otherwise discard the compressed result and fall back to the original (never emit broken JSON).
- Stats log write failure → warn, does not affect the request.

---

## 9. Test Strategy

A standalone crate, pure unit tests, no dependency on the codex runtime.

- **Each compressor**: `detect` truth table + `compress` snapshot tests (`insta`, real fixtures: real git diffs, real JSON tool outputs, real logs).
- **ContentRouter**: priority-match tests (when a chunk looks like a log yet also parses as JSON, who wins).
- **fail-open**: inject a fake compressor that panics, assert verbatim pass-through.
- **Threshold boundaries**: below `per_item_min_bytes` untouched; `enabled=false` fully passes through and `request` is byte-for-byte unchanged.
- **Irreversible but safe**: assert that post-compression size ≤ pre-compression size (never grows under compression), and that the placeholder marker is present.
- **Stats log**: an effective compression writes one CSV line, four columns, correct format; no compression writes nothing; a write failure does not panic.

---

## 10. Observability

- Inside `transform()`, use `tracing` to record the `saved_bytes` summary (debug level), without polluting normal output.
- The placeholder marker itself is a visible signal for both the model and humans.
- The CSV stats log supports offline analysis of compression effectiveness.
- v1 does not do persistent metrics / does not do token counting (YAGNI).

---

## 11. Module File Layout

```
zmod/llm-compress/
  Cargo.toml                  # name = "codez-llm-compress"
  src/
    lib.rs                    # transform() entry + enabled()
    config.rs                 # read [llm_compress]
    stats.rs                  # compression stats log log_compression()
    router.rs                 # ContentRouter + Compressor trait + Budget
    compress/
      mod.rs
      truncate.rs
      json.rs
      diff.rs
      log.rs
  tests/
    fixtures/                 # real diff/json/log samples
    snapshots/                # insta snapshots

patches/llm-compress.patch    # the two changes in core/Cargo.toml + client.rs
```

---

## 12. Key Decision Record

| Decision | Choice | Rationale |
|------|------|------|
| Connector/HTTP handling | Don't build our own; handled by the downstream router (switch/native) | llm-compress only transforms the request; transport is the downstream's responsibility, lowest risk |
| Relationship to llm-switch | Standalone crate, front-intercept at the integration point | Compression applies to all paths, including native OpenAI; not bound to a switch match |
| v1 compression scope | Content routing + 4 compressors (Json/Diff/Log/Truncate) | Covers the main shapes of tool output |
| Reversibility | Irreversible + conservative thresholds + fail-open | Simple and reliable; the placeholder marker explicitly signals omission |
| queryid | `responses_metadata.thread_id` | Readily reachable, identical to the rollout file name UUID, not crossed between child agents, and doesn't change codex signatures |
| Log format | CSV, four columns, no header | Concise, easy to parse |
