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
| Anthropic | explicit `cache_control` (max 4 breakpoints) OR top-level automatic | byte-identical prefix; reads via longest-prefix lookback (20-block window) | read 0.1x / write 1.25x (5m) | 1024 or 4096 tok by model |
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
Anthropic prompt caching on the byte-stable prefix that Part A guarantees.

### How Anthropic caching actually works (verified against official docs)

These mechanics drive the design and corrected an earlier flawed approach:

- **Prefix order is `tools → system → messages`.** A `cache_control` breakpoint writes
  **exactly one** cache entry: the prefix *ending at that block*. It does NOT cache any
  later block. So a breakpoint on the last tool caches tools only — `system` (which
  comes after) is not included.
- **Writes happen only at breakpoints; the cached content is REQUEST content** —
  including prior assistant turns that are now part of `messages`.
- **Reads are automatic longest-prefix lookback.** On each request the system hashes
  the prefix at the breakpoint and, if no match, walks backward block-by-block (window
  = **20 blocks**) looking for an entry a *prior request actually wrote*. It does not
  "discover" stable content behind the breakpoint — it only matches prior writes.
- Therefore the correct multi-turn pattern is to mark the **last block of the last
  message** every turn: turn N writes the full-prefix entry, turn N+1 (having appended
  < 20 blocks) lookback-hits turn N's entry, charging all prior conversation at read
  rate. (The earlier draft's "second-to-last message" reasoning was wrong: the last
  message is exactly where the marker belongs.)

### Decision: top-level automatic caching, gated per provider

Enable Anthropic's automatic caching by adding a single top-level field to the
translated request body **only when the target provider has opted in**:

```json
{ "cache_control": { "type": "ephemeral" } }
```

When present, Anthropic places the breakpoint on the last cacheable block and moves it
forward as the conversation grows — no manual breakpoint bookkeeping, and it is the
doc's first recommendation for multi-turn conversations.

**Why gated, not unconditional (compatibility):** automatic caching is supported only
on the Claude API, AWS Claude Platform, and Microsoft Foundry — **not Bedrock/Vertex**,
and there is **no guarantee** a third-party Anthropic-compatible gateway tolerates an
unknown top-level field. Since llm-switch's anthropic connector targets a
user-configured arbitrary `base_url`, unconditionally injecting `cache_control` could
turn a previously-working route into a **400 hard failure** — far worse than a missed
discount. So it is off by default and enabled per provider.

**Config:** add an optional `prompt_cache` boolean to a provider's table in
`~/.codex/config-zmod.toml`, default `false` (fail-safe, current behavior unchanged):

```toml
[llm-switch.providers.claude-sonnet]
connector     = "anthropic"
base_url      = "https://api.anthropic.com"
auth          = "x-api-key"
key_env       = "ANTHROPIC_API_KEY"
model         = "claude-sonnet-4-5"
prompt_cache  = true   # opt in: emit top-level cache_control for this provider
```

This adds `prompt_cache: bool` (`#[serde(default)]`) to `RawProvider` and `ProviderCfg`
(`config.rs:24,58`). The field is only meaningful for the anthropic connector; the chat
connector ignores it (DeepSeek/OpenAI auto-cache without any marker). When
`prompt_cache == true`, `build_anthropic_request` (`anthropic_req.rs:204`) adds the
top-level field after messages/tools/system are set. TTL: default 5 minutes (omit
`ttl`). The user is responsible for only enabling it on endpoints they know support
automatic caching — the default-off keeps unknown endpoints safe.

### Automatic-caching traps to honor (from the doc)

1. **Last block must not vary per request.** Automatic caching marks the last cacheable
   block; if it changes every turn, caching writes the wrong thing. codex's translated
   `messages` end with a user/tool_result block (this turn's content) — correct and
   stable for this purpose. The orphan-repair path appends a fixed-content placeholder
   `tool_result` at the user message tail (`anthropic_req.rs:178-192`); content is
   constant, does not introduce per-request variance. No action needed, but the
   determinism from Part A is what keeps the *earlier* prefix stable.
2. **Breakpoint-slot / TTL conflicts.** Automatic caching consumes one of the 4
   breakpoint slots. Since llm-switch emits no explicit `cache_control` elsewhere, there
   is no slot exhaustion and no mixed-TTL 400 risk. Keep it that way: do not add
   explicit breakpoints alongside automatic.
3. **Platform support is not universal — handled by the per-provider gate.** Automatic
   caching works on the Claude API, AWS Claude Platform, and Microsoft Foundry (beta);
   **Bedrock and Vertex do not support it**, and a third-party Anthropic-compatible
   gateway behind a user `base_url` may reject an unknown top-level field with a 400.
   We do NOT assume the field is silently ignored. Instead, `prompt_cache` defaults to
   `false`; the field is emitted only for providers the user explicitly opted in. No
   runtime platform detection, no retry logic — the safety comes from default-off.
4. **Min length still applies** (1024 / 2048 / 4096 tok by model). Below it, Anthropic
   processes without caching and returns no error. No token counting in llm-switch
   (out of scope). Accept and document.

### usage mapping fix

`anthropic_sse.rs` currently reads only `cache_read_input_tokens` and computes
`total_tokens = input_tokens + output_tokens` (`anthropic_sse.rs:217`). Anthropic's
`usage.input_tokens` counts only tokens *after the last cached block*; the full input
is `cache_read_input_tokens + cache_creation_input_tokens + input_tokens`. Fix:

- Read `cache_creation_input_tokens` in addition to `cache_read_input_tokens`.
- Map **`cache_read_input_tokens` → codex `cached_input_tokens`** (true cache hits;
  `cache_creation` is billed at 1.25x and is not a hit, so it is NOT reported here).
- Compute **`total_tokens = cache_read_input_tokens + cache_creation_input_tokens +
  input_tokens + output_tokens`** so totals reconcile with Anthropic billing.
- Keep codex `input_tokens` = Anthropic `input_tokens` (post-cache), matching how
  OpenAI reports uncached input. Document the mapping inline.

This also gives an observable cache signal: per the doc, both
`cache_creation_input_tokens` and `cache_read_input_tokens` being 0 means the prompt
was not cached (e.g. below min length, or endpoint without automatic support).

### `prompt_cache_key`

Remains dropped — Anthropic has no equivalent field and caching is driven by the
top-level `cache_control`. No change to `apply_field_downgrade` for this field.

### Verification

- **Gate test**: `prompt_cache = false` (default) → translated body has NO top-level
  `cache_control` (byte-identical to today's output, zero regression). `prompt_cache =
  true` → body has top-level `"cache_control": {"type": "ephemeral"}` and no per-block
  markers (single-mechanism guarantee).
- **Stability test (the real target)**: simulate a 3-turn conversation through
  `build_anthropic_request` (with Part A determinism applied upstream); assert the
  serialized prefix bytes of turn N+1 up to turn N's last block are byte-identical to
  turn N's serialized prefix — i.e. lookback *can* hit. This replaces the earlier,
  incorrect "marker on second-to-last" check.
- **usage test**: feed an Anthropic `message_start` with both
  `cache_read_input_tokens` and `cache_creation_input_tokens`; assert mapping
  (`cached_input_tokens` = read only; `total_tokens` reconciles) and that both-zero
  is surfaced as "uncached".
- `cargo nextest run -p codez-llm-switch` green.

## Out of scope

- OpenAI / DeepSeek request-side changes — both auto-cache; Part A's determinism is
  sufficient. No `cache_control` concept there.
- Token counting / min-length enforcement in llm-switch.
- Explicit per-block breakpoints, Anthropic 1h TTL, Bedrock/Vertex automatic-caching
  support, platform detection.
- Chat connector cache fields (DeepSeek auto-caches; nothing to mark).

## Rollout

Both parts are gated by their existing feature switches
(`[llm_compress]` / `[llm-switch]` in `~/.codex/config-zmod.toml`). Part A adds no
config. Part B adds one optional per-provider field `prompt_cache: bool` (default
`false`); with it unset, behavior is byte-identical to today (zero regression for
existing providers). Each part lands as changes to its own zmod crate; `patches/*.patch`
are unaffected (no new call sites or signatures). Part A and Part B can ship
independently, but Anthropic caching is only effective once both are in **and** a
provider sets `prompt_cache = true`.
