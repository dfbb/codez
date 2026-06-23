# TOON Encoding Replaces csv-schema — Design

Date: 2026-06-22
Status: Approved (pending spec review)
Scope: `zmod/llm-compress` only (no `patches/` change)

## Background

The llm-compress layer compresses **tool-output content** at the LLM request
boundary — the JSON text inside `FunctionCallOutput` / `CustomToolCallOutput`
items that the model reads. Today the JSON family of compressors expresses
homogeneous object arrays in a homemade `_schema` / `_rows` form (`schema.rs`),
shared by `JsonCompressor` and `TabularCompressor`.

[TOON](https://github.com/toon-format/toon-rust) (Token-Oriented Object
Notation) is a line-oriented, indentation-based encoding of the **full JSON
data model**. Homogeneous arrays become a header plus bare rows:

```toon
users[2]{id,name}:
  1,Ada
  2,Linus
```

TOON is the standardized, more compact equivalent of our `_schema`/`_rows`
form, and the `toon-format` crate round-trips losslessly over any
`serde_json::Value` via `encode_default` / `decode_default`.

## Goal

Re-encode JSON tool-output content as TOON to reduce token usage. The wire
envelope stays JSON and **the model still emits JSON as always** — only the
content string the model *reads* becomes TOON.

## Architecture: TOON replaces csv-schema (not coexist)

- **Delete** `src/compress/schema.rs` (`to_schema_form`, the `_schema`/`_rows`
  internal form) and its test `tests/schema_test.rs`.
- **`JsonCompressor`**: `serde_json::from_str` → `encode_default(&value)` →
  TOON text. No more RLE dedup (`_llm_dup_prev`), no more `_schema`/`_rows`.
- **`TabularCompressor`**: parse the CSV/TSV/Markdown table into a
  `Value` (array of objects, as today), then `encode_default(&value)` → TOON.
- **New `ContentKind::Toon`**: csv-schema output was valid JSON (`kind==Json`);
  **TOON is not JSON**, and it must NOT reuse `kind==Text` either. The
  orchestrator (`lib.rs`) attaches a CCR pointer whenever
  `kind==Text && (pre_lossy || comp_lossy)`. So a `Text` TOON product, when
  preprocessing was lossy (e.g. progress lines stripped before a still-parseable
  JSON blob), would become `TOON + [llm-compress: …]` text — no longer decodable
  TOON, and the round-trip self-check (run inside the compressor, before CCR
  attach) would not catch it.
- Therefore introduce a distinct `ContentKind::Toon` (always `lossy==false`).
  In the orchestrator, treat `Toon` like `Json`: **never attach CCR**. It also
  skips the `json_valid` write-back gate (that gate re-parses as JSON and would
  reject TOON); TOON's validation is its own internal round-trip self-check
  (below). This matches the existing "structured product ⟹ no CCR" semantics of
  the JSON path and avoids threading `pre_lossy` into the compressors.

### Dropped: RLE consecutive dedup

`JsonCompressor`'s RLE step folded consecutive-identical array elements into
`{"_llm_dup_prev": N}` markers. Those marker objects break array homogeneity,
so TOON could no longer tabularize a folded array — the two optimizations
fight. RLE is dropped entirely (markers and logic). Consecutive-identical
elements in tool output are rare; TOON's tabular form is the larger win.

## Data flow (single content segment)

```
tool-output JSON string
  → serde_json::from_str → Value           (Err → Unchanged, fail-open)
  → encode_default(&value) → toon: String  (Err → Unchanged)
  → size check FIRST: toon.len() < original.len() ?
        (not smaller → Unchanged; this guards the saved subtraction below)
  → round-trip self-check:
        decode_default::<Value>(&toon) == original Value ?
        (not-equal or Err → Unchanged, fail-open)
  → kind=Toon, lossy=false, saved = original.len() - toon.len()  (> 0 here)
  → router size gate + write-back
```

The "smaller than original" check happens **before** computing `saved` and
before returning `Compressed`. `saved = original.len() - toon.len()` only runs
on the branch where `toon.len() < original.len()`, so it cannot underflow. As a
belt-and-suspenders measure the code uses `saturating_sub` and returns
`Unchanged` when `saved == 0`.

## Round-trip safety self-check

Because TOON is the model's **only** view of that tool output, and the
orchestrator never re-parses a `kind==Toon` product, both `JsonCompressor` and
`TabularCompressor` MUST, after producing TOON and before returning, run
`decode_default::<Value>(&toon)` and compare it to the original `Value`. Any
decode error or inequality → return `Unchanged` (fall back to the original
text). This is the TOON path's own fail-open gate, equivalent to the JSON
path's `json_valid` write-back gate.

## detect / yield-to-Truncate (preserved)

`JsonCompressor.detect` keeps its current discipline: parses to object/array
**and** the TOON product is `<= truncate.max_bytes` → claim; otherwise return
false and let `TruncateCompressor` handle it. `TabularCompressor.detect` is
analogous (claim only when the TOON product is smaller than the original).

## Determinism (cache compatibility)

`encode_default` is a pure function of the input `Value`, and
`serde_json::Value` has a deterministic key order, so the TOON path keeps
compression a pure function of content — preserving the prompt-cache stability
guarantee from the 2026-06-22 prompt-caching-compat design. The existing
`tests/cache_stability_test.rs` style (multi-run byte-identical) is extended to
cover the TOON path.

## Configuration

`[llm_compress.json]` table:

- **Add** `use_toon: bool`, `#[serde(default)]`, default `true`. Single master
  switch for JSON **and** Tabular TOON encoding.
- **Remove** `csv_schema` (dies with `schema.rs`).
- **Remove** the entire `[llm_compress.tabular]` table, including
  `tabular.enabled` and the `TabularCfg` struct. Tabular is now gated by
  `use_toon`.

Switch semantics:

- `use_toon = true` (default): `JsonCompressor` and `TabularCompressor` both
  encode the parsed `Value` to TOON.
- `use_toon = false`: both `detect` return false and do not claim. JSON / table
  content passes through verbatim; oversized content still falls to
  `TruncateCompressor`.

Example `~/.codex/config-zmod.toml`:

```toml
[llm_compress.json]
use_toon = true
```

## Dependency

`zmod/llm-compress/Cargo.toml`: add `toon-format = "=<pinned-version>"` (exact
pin per CLAUDE.md dependency policy). During implementation, verify it compiles
under the workspace toolchain (Rust 1.95.0).

## Testing

- **Unit**: homogeneous object array, nested object, scalar array each encode to
  the expected TOON; round-trip self-check rejects a deliberately
  non-round-trippable / oversized input (falls back to original); TOON larger
  than original → not claimed (yields to Truncate).
- **Determinism**: same input encoded multiple times yields byte-identical
  output (reuse the cache-stability test pattern).
- **Switch**: `use_toon = false` → `JsonCompressor` / `TabularCompressor`
  `detect` return false.
- **CCR isolation (orchestrator)**: a `kind==Toon` product with lossy
  preprocessing (e.g. progress lines stripped) is written back as **bare TOON,
  no `[llm-compress: …]` CCR pointer appended** — assert the result still
  `decode_default`s. This is the regression test for the `ContentKind::Toon`
  treatment in `lib.rs`.
- **Rewrite** existing `tests/json_test.rs` and `tests/tabular_test.rs` to
  assert TOON products instead of `_schema`/`_rows`.
- **Delete** `tests/schema_test.rs`.
- **Update** `tests/config_test.rs` (drop `csv_schema`, add `use_toon`),
  `tests/parity_test.rs`, and the `tests/fixtures/inherited/` manifest /
  fixtures that reference `json` / `tabular` expected outputs.

## Impact

`src/router.rs` (add `ContentKind::Toon` variant),
`src/lib.rs` (orchestrator: treat `Toon` like `Json` — no CCR, skip
`json_valid`), `src/compress/json.rs`, `src/compress/tabular.rs`, delete
`src/compress/schema.rs`, `src/compress/mod.rs` (drop `pub mod schema;`),
`src/config.rs` (switch rename + drop `TabularCfg`), `Cargo.toml`. The codez
patch is unaffected — this is a pure zmod-internal change.
```
