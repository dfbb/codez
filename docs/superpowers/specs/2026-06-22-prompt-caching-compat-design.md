# Design: Prompt Caching Compatibility for llm-compress & llm-switch

Date: 2026-06-22
Status: Approved (design phase)
Scope: Two independent cache-correctness fixes shipped under one spec.

## Background

codex relies on upstream **prefix caching** to keep multi-turn conversations cheap.
Every turn re-sends the full history; the upstream charges the unchanged prefix at a
heavily discounted rate (OpenAI/DeepSeek auto prefix cache, Anthropic explicit
`cache_control` breakpoints). The hard rule shared by all three upstreams: **the
prefix must be byte-for-byte identical to the previously sent prefix, or the cache
entry — and everything after it — is invalidated.**

Two codez features interact badly with this:

- **Problem A — llm-compress breaks prefix stability.** Compression rewrites tool
  outputs in place across the *entire* `input` (including history) every turn. Two
  drift sources make the same history item compress to *different* bytes on different
  turns, invalidating the cached prefix from that item onward.
- **Problem B — llm-switch → Anthropic never enables caching.** The Anthropic
  connector emits no `cache_control` field and silently drops `prompt_cache_key`.
  Every request routed to Claude recomputes the whole history at full price.

The two fixes are orthogonal but complementary: A keeps codex's `input` prefix
byte-stable; B marks that stable prefix as cacheable on the Anthropic wire. For
Anthropic, **both** are required — A alone leaves Anthropic uncached; B alone is
defeated by A's nondeterminism.

## Verified facts (investigation)

Cache mechanics (verified against official docs, 2025-2026):

| Upstream | Enable | Hit condition | read / write multiple | Min cacheable |
| --- | --- | --- | --- | --- |
| OpenAI | automatic | exact prefix match | read 0.1x (GPT-5 era) | 1024 tok |
| Anthropic | explicit `cache_control` (max 4 breakpoints) | byte-identical prefix up to breakpoint | read 0.1x / write 1.25x (5m) | 1024 or 4096 tok by model |
| DeepSeek | automatic (disk) | prefix-unit match | read ~0.02x | n/a |

codex integration (verified in code):

- `prompt_cache_key = thread_id` (`core/src/client.rs:418`), session-stable.
- llm-compress runs at `core/src/client.rs:1324`, **before** llm-switch routing, with
  `client_setup.api_provider` and `model_provider_id` already resolved.
- llm-compress iterates the **entire** `request.input` (`zmod/llm-compress/src/lib.rs:63`),
  i.e. history is recompressed every turn.
- `queryid` passed to llm-compress is `responses_metadata.thread_id`
  (`patches/llm-compress.patch:23`) — session-stable, NOT a per-request UUID. CCR
  placeholder paths are therefore stable and do **not** break the prefix.
- Anthropic connector emits no `cache_control`; drops `prompt_cache_key`
  (`zmod/llm-switch/src/connector/anthropic_req.rs:457`).
- Anthropic SSE reads `cache_read_input_tokens` but not `cache_creation_input_tokens`
  (`zmod/llm-switch/src/connector/anthropic_sse.rs:92`).

## Part A — llm-compress determinism

### Core claim

llm-compress breaks the prefix cache not because it compresses history, but because
the same history item compresses to *different bytes* on different turns. Make
compression a **pure function of the item's own content**, and each tool output is
compressed once (when it first appears as the tail), then deterministically reproduces
the identical bytes on every subsequent turn as history. The prefix stays byte-stable
across all three upstreams, while **all history-compression savings are retained**.

There is no "first-turn invalidation": a tool output is new content the turn it
appears (a cache miss regardless of compression), and from the next turn on its
compressed form is byte-identical, so the prefix hits.

### Drift-source audit

| Source | Location | Drifts? | Action |
| --- | --- | --- | --- |
| query_terms (last user message keywords) | `query.rs:14` → `line_score` in Search/Log | **Yes** — changes every turn | Remove query weighting |
| min_total_bytes global gate | `lib.rs:60` (judged on whole-request total) | **Yes** — phase change when total crosses 4096 | Drop the global gate; keep per-item gate only |
| cmd_index (command hint) | `command.rs:30`, keyed by call_id | No — call_id is a stable content key | Unchanged |
| CCR placeholder path | `ccr.rs` (queryid=thread_id + call_id + content hash) | No — all stable | Unchanged |
| Search/Log sort tie-break | `search.rs:97/127` | No — stable sort + BTreeSet + first-appearance order | Unchanged (becomes deterministic once query removed) |

### Changes

1. **Remove query weighting (Search/Log).** Drop the `query::extract` call from the
   request context; `line_score` no longer takes query terms (or always receives an
   empty term list). Search/Log lossy selection scores on content features only (line
   length, log level, structural density). This is the agreed tradeoff: slightly worse
   selection relevance in exchange for byte-stable, cache-friendly, testable output.
   `query.rs` and the query plumbing in the request context are removed.
2. **Drop the global `min_total_bytes` gate (`lib.rs:60`).** Keep only the per-item
   `per_item_min_bytes` gate, which is judged on a single item's byte count and is
   therefore constant for a given history item across turns. Removing the global gate
   eliminates the phase-change drift and is strictly more cache-friendly.
3. **Orphan cleanup.** Remove imports/fields/functions that changes 1-2 make unused
   (e.g. `RequestCtx.query_terms`, `query` module). Do not touch unrelated code.

### Why determinism beats "compress only the tail / freeze history"

Freezing history (compress only the newest tool output, byte-freeze older ones)
*manufactures* a tail→history form transition that itself needs a snapshot mechanism
to avoid invalidation. Whole-history deterministic compression has no such boundary:
the tail's compressed form on turn N is identical to its history form on turn N+1 by
construction. Simpler and retains more savings.

### Verification

- **Determinism test**: build two requests sharing an identical history segment
  (turn N's tail = turn N+1's history item), run `transform` on both, assert the
  shared item's `output.body` is byte-identical between the two runs.
- **Prefix-stability test**: simulate a 3-turn conversation, assert that for each turn
  `input[..prev_len]` after compression equals the previous turn's compressed input
  byte-for-byte.
- Existing Search/Log/JSON compressor tests updated to reflect query-free scoring.
- `cargo nextest run -p codez-llm-compress` green (via the dev symlink workspace
  member, per CLAUDE.md Case B).

## Part B — llm-switch → Anthropic prompt caching

### Goal

Make requests routed to Claude (main session or purpose-routed `review`) benefit from
Anthropic prompt caching by emitting `cache_control` breakpoints on the byte-stable
prefix that Part A guarantees.

### Breakpoint placement: tools + sliding history

The translated Anthropic body has a stability gradient (most → least stable):
`tools` → `system` → `messages`. Anthropic builds the cache prefix in exactly this
layer order, so a breakpoint covers the whole prefix up to and including its block.

Use **2 of the 4** available breakpoints (leaving headroom):

1. **`tools` breakpoint** — `cache_control` on the **last tool definition** in the
   `tools` array. Because the prefix is `tools → system`, this one breakpoint caches
   both the tool definitions and the `system` block (both session-stable).
2. **Sliding history breakpoint** — `cache_control` on the last content block of the
   **second-to-last message** in `messages`. Anchoring at the second-to-last (not the
   last) message ensures the marked prefix is content already sent and cached on the
   previous turn; the last message is this turn's freshly appended content (a
   necessary miss). Each turn: only the new tail is a cache write, all history is a
   cache read.

If `tools` is empty, place breakpoint 1 on `system` instead (set
`system` as a structured block array with a trailing `cache_control`). If `messages`
has fewer than 2 messages, skip the sliding breakpoint (nothing stable to cache yet).

### Anthropic `cache_control` wire format

- Block-level marker: append `"cache_control": {"type": "ephemeral"}` to the target
  content block object (tool def / system block / message content block).
- TTL: default (5 minutes). Do not opt into 1h (`"ttl":"1h"`) — extra write cost,
  not justified for interactive sessions.
- This requires emitting `system` and `tools` entries as objects that can carry the
  field. Tool defs are already objects (`map_tools`). `system` is currently a bare
  string (`anthropic_req.rs:213`); to mark it, it must become a structured form
  `[{"type":"text","text":..., "cache_control":{...}}]` **only when** it is the
  breakpoint target (tools empty); otherwise leave it as a string to avoid needless
  prefix changes.

### Min-length guard

Anthropic silently skips caching below the per-model minimum (1024 or 4096 tokens).
No token counting in llm-switch (out of scope per v2). Marking below-threshold prefixes
is harmless (Anthropic ignores the marker, no error). Accept this; document it.

### usage mapping fix

`anthropic_sse.rs` currently reads only `cache_read_input_tokens` and computes
`total_tokens = input_tokens + output_tokens` (`anthropic_sse.rs:217`). Anthropic's
`usage.input_tokens` counts only tokens *after the last breakpoint*; the full input is
`cache_read_input_tokens + cache_creation_input_tokens + input_tokens`. Fix:

- Read `cache_creation_input_tokens` in addition to `cache_read_input_tokens`.
- Map **`cache_read_input_tokens` → codex `cached_input_tokens`** (true cache hits;
  `cache_creation` is billed at 1.25x and is not a hit, so it is NOT reported here).
- Compute **`total_tokens = cache_read_input_tokens + cache_creation_input_tokens +
  input_tokens + output_tokens`** so totals reconcile with Anthropic billing.
- Keep codex `input_tokens` = Anthropic `input_tokens` (post-breakpoint), matching how
  OpenAI reports uncached input. Document the mapping inline.

### `prompt_cache_key`

Remains dropped — Anthropic has no equivalent field and caching is driven by the
explicit breakpoints. No change to `apply_field_downgrade` for this field.

### Verification

- **Wire-format test**: translate a multi-message request, assert the `tools` array's
  last entry carries `cache_control`, and the second-to-last message's last block
  carries `cache_control`.
- **Edge cases**: empty tools → system carries the marker; <2 messages → no sliding
  breakpoint; tools present + ≥2 messages → exactly 2 breakpoints.
- **usage test**: feed an Anthropic `message_start` with both
  `cache_read_input_tokens` and `cache_creation_input_tokens`; assert mapping
  (`cached_input_tokens` = read only; totals reconcile).
- `cargo nextest run -p codez-llm-switch` green.

## Out of scope

- OpenAI / DeepSeek request-side changes — both auto-cache; Part A's determinism is
  sufficient. No `cache_control` concept there.
- Token counting / min-length enforcement in llm-switch.
- Anthropic 1h TTL, automatic top-level cache mode, Bedrock/Vertex specifics.
- Chat connector cache fields (DeepSeek auto-caches; nothing to mark).

## Rollout

Both parts are gated by their existing feature switches
(`[llm_compress]` / `[llm-switch]` in `~/.codex/config-zmod.toml`); no new config.
Each part lands as changes to its own zmod crate; `patches/*.patch` are unaffected
(no new call sites or signatures). Part A and Part B can ship independently, but
Anthropic caching is only effective once both are in.
