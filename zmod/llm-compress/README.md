# codez-llm-compress

One of codez's zmod features. **Before** codex sends an LLM request upstream, it performs an in-process, irreversible but conservative compression on the already-assembled `ResponsesApiRequest`, reducing the token volume sent upstream, and records every effective compression into a CSV statistics log.

- **Package name**: `codez-llm-compress` (`publish = false`)
- **lib target**: `codez_llm_compress`
- **Corresponding patches**: build integration `patches/001-build.patch` (shared) + code hook `patches/003-llm-compress.patch`
- **Design basis**: `docs/superpowers/specs/2026-06-20-llm-compress-design.md`

## What it does

After codex assembles the `ResponsesApiRequest` in `stream_responses_api` in `core/src/client.rs`, and before it actually sends it, it calls this crate's single entry point `transform()`. `transform` traverses the **tool output** text in the request in place, picks a compressor by content type, compresses large chunks of text exceeding the threshold, and replaces the elided parts with an explicit placeholder marker `[llm-compress: …]`.

It **only** transforms request content; it does not switch upstreams, does not modify the response stream, does not do reversible retrieval, and does not do token counting. It is fully transparent to downstream (native `stream_request` / sibling zmod `llm-switch` / SSE parsing / error handling): when disabled it is equivalent to a zero-change path.

Relationship to `llm-switch`: both hook into the same codex integration point but have orthogonal responsibilities. llm-compress is a **pre-interception** stage—compress first, then route—and applies to **all** request paths (including the native OpenAI responses path), independent of whether llm-switch hits a route.

## Compression scope (only touches tool output)

Only the `output` text of two `ResponseItem` variants is processed; all other variants are left untouched:

- `FunctionCallOutput`
- `CustomToolCallOutput`

Text extraction rules:

- `FunctionCallOutputBody::Text(s)` → compress `s`.
- `FunctionCallOutputBody::ContentItems(items)` → for each item, **only compress** the `text` of `InputText{text}`; `InputImage` / `EncryptedContent` are neither read nor modified, and never flattened.

## Four compressors and routing

The internal `ContentRouter` runs `detect` in **fixed priority** order; the first one to claim the content executes `compress`; `Truncate` is always the fallback.

| Priority | Compressor | detect basis | compress strategy |
|---|---|---|---|
| ① | **Json** | Parses via `serde_json` into an **object/array** (top-level scalars are yielded to downstream) | In-structure compression: sample long arrays (keep head and tail + `"…(N more)"`), truncate over-deep subtrees to `"…"`; the output must pass a re-parse check, falling back to the original text on failure. Never does text-level truncation. |
| ② | **Diff** | Contains `@@…@@` hunk headers / `diff --git` / paired `--- ` `+++ ` | Keeps all changed lines and structural headers; redundant context within a hunk is folded into `[llm-compress: 略 N 行上下文]`. |
| ③ | **Log** | Line count ≥ 8 and contains timestamps / stack traces / consecutive duplicate lines | Consecutive duplicate lines are folded into `[llm-compress: 上一行 ×N]`; then head/tail are kept and the middle section folded into `[llm-compress: 略 N 行]`. |
| ④ | **Truncate** | Always true (fallback) | Strip ANSI → keep head/tail lines, middle becomes `[llm-compress: 略 N 行 / M 字节]`; if still over `max_bytes`, hard-truncate at a UTF-8 character boundary. |

The placeholder marker is uniformly `[llm-compress: …]` (JSON is the exception: the placeholder is carried as a valid JSON value), making it clear to the model that something has been elided here. Compression is **irreversible**, but conservative—it only compresses large items over the threshold, and guarantees the post-compression size ≤ the pre-compression size.

## fail-open (compression never blocks the request)

`transform()` returns `()` (not a `Result`), ruling out "compression failure blocks the request" at the type level:

- If any compressor panics in `detect`/`compress` → `catch_unwind` catches it → that fragment passes through as its original text.
- config parse failure → treated as `enabled = false` + warn, takes the zero-change path.
- JSON post-compression parse failure → discard the compression result, fall back to the original text (does not emit broken JSON).
- statistics log write failure → only warns, does not affect the request.

## Configuration

Reads the `[llm_compress]` section of `~/.codex/config-zmod.toml` (same file as llm-switch, separate section). **A missing section or `enabled = false` → fully disabled** (fail-safe). `load()` reads and caches once per process, rather than re-reading from disk on every request.

```toml
[llm_compress]
enabled = false                 # disabled by default; only takes effect when set to true
min_total_bytes = 4096          # if the total tool-output text in a request is below this → skip entirely (don't bother with small requests)
per_item_min_bytes = 1024       # if a single text fragment is below this → don't compress (conservative threshold)

[llm_compress.truncate]
head_lines = 50                 # keep the first N lines
tail_lines = 50                 # keep the last N lines
max_bytes  = 16384              # per-item post-compression byte cap; hard-truncate if exceeded

[llm_compress.json]
max_array_items = 20            # arrays longer than this → sample, keep head/tail + count
max_depth = 6                   # subtrees deeper than this → truncate to "…"

[llm_compress.diff]
context_lines = 3               # context lines kept before and after each hunk's changed lines

[llm_compress.log]
dedup_repeats = true            # fold consecutive duplicate lines into "[llm-compress: 上一行 ×N]"
```

All fields have default values (see the comments above), used when omitted.

## Statistics log

- **File**: `~/.codex/log/llm-compress.log` (the directory is created if it does not exist; append mode).
- **Trigger**: one line is appended after a request achieves **effective compression** overall (`saved_bytes > 0`); disabled / uncompressed states are not recorded.
- **Format**: CSV, four columns, no header, no quotes:

  ```
  时间戳,queryid,压缩前字节,压缩后字节
  ```

  ```
  2026-06-20T08:15:30Z,019e3995-5cd9-75a2-b487-f7959835f69e,18432,5120
  ```

| Column | Source |
|---|---|
| timestamp | RFC3339 UTC, second precision (`chrono`) |
| queryid | `responses_metadata.thread_id` (exactly matches the rollout file name UUID) |
| pre-compression bytes | total tool-output text bytes at the transform entry |
| post-compression bytes | total tool-output text bytes at the transform exit |

The byte measure is the sum of tool-output text bytes (the actual target the compressors act on), not the whole serialized request bytes.

## Module layout

```
zmod/llm-compress/
  Cargo.toml
  src/
    lib.rs            # transform() entry + enabled() + ResponseItem traversal/text extraction
    config.rs         # reads [llm_compress]; load() caches per process, load_from() for test injection
    stats.rs          # CSV statistics log log_compression()
    router.rs         # Compressor trait + Budget + ContentRouter (fixed priority + fail-open)
    compress/
      mod.rs
      json.rs         # JsonCompressor (in-structure compression + parse check)
      diff.rs         # DiffCompressor (unified diff context folding)
      log.rs          # LogCompressor (duplicate-line folding + head/tail)
      truncate.rs     # TruncateCompressor (fallback: strip ANSI + head/tail + hard truncation)
  tests/              # per-compressor detect/compress + router priority/fail-open + transform end-to-end
```

## Build and test

This crate has a reverse dependency on `codex-api` / `codex-protocol` (CLAUDE.md "Case B"); it must be symlinked into the codex-rs workspace as a real member in order to run `[dev-dependencies]` and `tests/*.rs` integration tests. Development scaffolding (dev-only, not committed into the codex-rs subtree):

```bash
# in the repository root
ln -s ../zmod/llm-compress codex-rs/llm-compress     # symlink (already covered by .gitignore)
# add one line at the end of the [workspace] members in codex-rs/Cargo.toml:
#     "llm-compress",

cd codex-rs
cargo test -p codez-llm-compress           # run all tests
cargo clippy -p codez-llm-compress --all-targets
```

The symlink, the member line in `codex-rs/Cargo.toml`, and the build-generated `codex-rs/Cargo.lock` changes are all dev-only scaffolding; keep them uncommitted and **out of** any patch.

## Production hookup (patch)

Build integration is carried by the shared `patches/001-build.patch` (adds the `core/Cargo.toml` path dependency); the code hookup is expressed against codex-rs by `patches/003-llm-compress.patch` (single point, does not change any codex function signature):

1. `codex-rs/core/Cargo.toml` adds the external path dependency `codez-llm-compress = { path = "../../zmod/llm-compress" }` (not added to workspace members).
2. `codex-rs/core/src/client.rs` inserts the queryid retrieval + `transform()` call after `prepare_response_items_for_request` and before `record_started`.
