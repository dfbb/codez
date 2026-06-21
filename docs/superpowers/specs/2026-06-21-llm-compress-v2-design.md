# llm-compress v2 Design Document — Compression Capability Expansion

**Date**: 2026-06-21
**crate**: `codez-llm-compress` (`zmod/llm-compress/`, v1 already exists)
**Status**: Design approved, implementation plan pending
**Prerequisite**: v1 design in `docs/superpowers/specs/2026-06-20-llm-compress-design.md` (already landed: the four compressors Json/Diff/Log/Truncate + ContentRouter + fail-open + CSV statistics log)

> Language convention: all conversations and documents in this repository use Chinese uniformly.

---

## 1. Goals and Boundaries

Building on v1, this expands the compression capabilities of `codez-llm-compress`, benchmarked against the reference implementations `../3rd/compress/headroom` (content routing + multiple compressors) and `../3rd/compress/rtk` (command-output filtering pipeline), and adds a CCR (original-text retrieval) mechanism.

**Core constraints (inherited from v1, unchanged)**:

- Single entry point `transform(&mut request, &api_provider, queryid)`, returns `()`, **signature unchanged**.
- **No changes to the integration-point patch**: all inputs for the new capabilities (command context, query keywords) are extracted from the `request` already held by transform — no new touchpoints added to `core/src/client.rs`, the codex tool system is left untouched.
- Only the `output` text of the two variants `FunctionCallOutput` / `CustomToolCallOutput` is processed; the text-extraction rules are unchanged (`Text(s)` compresses `s`; `ContentItems` compresses only `InputText.text`, images / encrypted content are left untouched, never flattened).
- **fail-open throughout**: if anything goes wrong at any stage, fall back to the original text / skip — never block the request.
- The placeholder marker is uniformly `[llm-compress: …]`. Compressed size ≤ pre-compression size. UTF-8 safe. Non-test code does not use `unwrap`/`expect` (except at the catch_unwind boundary).

**Scope (included this round, 12 items + shared primitives)**:

| Group | Item | Form | Removes substantive content? | Attaches CCR |
|---|---|---|---|---|
| A New compressors | Search (grep/ripgrep) | New Compressor | Drops matches (lossy) | ✓ |
| | Tabular (CSV/TSV/MD tables) | New Compressor | No (format re-construction, content preserved) | ✗ |
| B Upgrades | Log → error-first + template mining (RLE) | Rewrite log.rs | Line dropping is lossy / template folding does not drop | Line-dropping part |
| | JSON +csv-schema +consecutive RLE dedup | Incremental json.rs | No (only non-removing steps; if removal is needed, hand off to Truncate) | ✗ |
| C New techniques | base64/blob folding | preprocess-layer blob_fold segment (sole location) | Drops (replaced with placeholder) | ✓ |
| | Error-output protection | router front gate | — not compressed | — |
| D rtk | Generic preprocessing (progress bars/blob/blank lines/over-long lines/consecutive duplicates) | New preprocess.rs | Progress-bar/blob dropping and over-long-line truncation are lossy; blank-line normalization/consecutive folding do not drop | Removed segments attach |
| | Command-aware routing (call_id→command name, routing hint only) | New command.rs | — | — |
| E CCR | Persist to disk + Text-path placeholder, dual-limit cleanup | New ccr.rs | — | — |
| Shared | Scoring (content features + last user message weighting) | New score.rs | — | — |
| Shared | Query keyword extraction | New query.rs | — | — |

> **`lossy` definition (used throughout, fix #2) = whether substantive content was removed (semantic definition, not byte-level)**. Pure format re-construction (JSON minify, csv-schema, table-to-JSON, consecutive blank-line normalization, consecutive duplicate/item RLE) preserves all content → `lossy=false`, no CCR attached; sampling/line-dropping/match-dropping/truncation/over-depth/base64 folding remove content → `lossy=true`, CCR attached. See §4.0 for details.

**Out of scope (deliberately excluded, conflicting with the thin / irreversible / hot-path positioning)**: Code (tree-sitter AST), Kompress (ML model), HTML extraction (trafilatura), Magika ML detection, message-level trimming / sliding window, CacheAligner, Net-Cost Gate, per-auth-mode differentiated strategies, cross-session learning (TOIN), token counting.

---

## 2. Overall Architecture and Data Flow

Reuses v1's `ContentRouter` + `Compressor` trait + fail-open. New routing priority (specialized formats first, Search before Log to keep grep from being grabbed):

```
Json → Search → Diff → Tabular → Log → Truncate
```

**transform orchestration (rewrite lib.rs)**: first build a one-time request context (including a **mutable CCR registry**), then process item by item.

```
transform(request, _provider, queryid):
  cfg = config::load()                         // OnceLock cache (already in v1)
  if !cfg.enabled { return }
  ctx = RequestCtx {
      queryid,                                 // CCR directory name
      query_terms: query::extract(request),    // S2: keywords from the last user message (one-time)
      cmd_index:  command::index(request),     // D2: call_id → CommandHint (one-time)
      ccr: RefCell<CcrRegistry>,               // #8: mutable, records (call_id,fragment_hash)→persisted file path (one file per text fragment)
  }
  total_before = total_text_bytes(&request.input)
  for item in request.input.iter_mut():        // still touches only the two variants
      compress_item(item, &ctx, &cfg)
  total_after = total_text_bytes(&request.input)
  if total_after < total_before { stats::log_compression(queryid, before, after) }
```

**Single text-fragment processing chain (①–⑥)**:

```
compress_in_place(s, ctx, cfg, call_id):
  cmd = ctx.cmd_index.get(call_id)                      // ① command name (Option)
  if protect::should_protect(s, cmd, cfg) { return }    // ② error-protection gate, skip on hit
  (pre, pre_lossy) = preprocess::run(s, &cfg.preprocess)  // ③ rtk preprocessing, returns (text, whether substantive content was removed)
  candidate =
    match router.compress_text(&pre, &Budget{cfg, ctx, cmd}):   // ④⑤ routing + compression (detect also takes budget)
      Some((new, comp_lossy, kind)) =>
        lossy = pre_lossy || comp_lossy
        // lossy=true ⟹ kind=Text (§4.0); JSON/Tabular are always lossy=false and never enter this branch
        if lossy { ccr::attach(new, original=s, ctx, call_id, &cfg.ccr) } else { new }  // ⑥ attach does not take kind
      None =>                                            // router did not compress, keep only the preprocessing result
        if pre_lossy { ccr::attach(pre, original=s, ctx, call_id, &cfg.ccr) } else { pre }
  // #4: a [unified] second size check before final write-back — not just inside attach.
  // The lossless preprocessing branch (blank-line normalization / RLE count placeholder) may also grow for small inputs.
  if candidate.len() <= s.len() { *s = candidate }      // otherwise keep the original, preserving "compressed ≤ pre-compression" (§1)
```

> **New behavior (needs tests)**: the preprocessing result is kept even if the router did not compress; **if preprocessing removed substantive content (dropped progress bars/blobs/truncation), CCR is likewise attached** (#5). Pure format-reconstruction preprocessing segments (consecutive blank-line normalization, consecutive duplicate-line folding) do not trigger lossy. **Final write-back uniformly passes through the `candidate.len() <= original.len()` gate (#4)**; if unsatisfied, fall back to the original text.
>
> **protect has the highest priority (#7, deliberate design)**: `protect::should_protect` runs **before all processing**, and on a hit it `return`s immediately, so the fragment is **kept byte-for-byte unchanged in its entirety** — including harmless preprocessing such as ANSI/blank lines/blob/duplicate lines, **none of which is performed**. Rationale: error/exception output is critical for diagnosis, and any change (even ANSI stripping) could interfere with the model's judgment, so small error outputs that hit the protection gate are passed through completely verbatim. This is a deliberate trade-off, prioritized over "preprocessing is a generic layer".

**Unified CCR attachment principle**: any processing (preprocessing or a compressor) that removed **substantive content** (`lossy=true`) attaches CCR; pure format reconstruction (`lossy=false`) does not. The `lossy` definition is in §4.0 (semantic, not byte-level). Handled uniformly by the orchestration-layer `ccr::attach`: because **`lossy=true ⟹ kind=Text`** (§4.1 invariant), attach **only handles Text products and only produces bare Text placeholders** (does not take kind, no JSON injection). There are two size gates: the placeholder-concatenation check inside `attach` (§4.7) + the orchestration-layer final write-back check (#4, §2 processing chain).

**Module layout (new)**:

```
src/
  lib.rs            # rewrite: orchestration + RequestCtx
  config.rs         # extend: add sub-table Defaults
  preprocess.rs     # new: rtk generic preprocessing segments
  command.rs        # new: call_id→CommandHint index + routing hint
  query.rs          # new: extract keywords from the last user message
  score.rs          # new: shared scoring (content features + query weighting)
  protect.rs        # new: error-output protection decision
  ccr.rs            # new: persist to disk + placeholder + LRU cleanup
  router.rs         # change: CompressOutcome adds lossy+kind (ContentKind); compress_text returns Option<(String,bool,ContentKind)>
  compress/
    mod.rs
    schema.rs       # new: csv-schema in-band representation, shared module (used by JSON + Tabular)
    search.rs       # new: SearchCompressor
    tabular.rs      # new: TabularCompressor
    json.rs         # upgrade: decision hand-off inside detect; csv-schema + consecutive RLE dedup (neither removes content); remove v1 sampling/over-depth trimming
    log.rs          # rewrite: template mining (no removal) + level-score retention (line dropping)
    diff.rs         # unchanged (lossy products attach CCR via orchestration)
    truncate.rs     # change: detect takes budget; truncation products attach CCR via orchestration (blob folding moved to preprocess)
```

---

## 3. Feature → Reference Source-File Mapping

> Path base `../3rd/compress/` (i.e. `/Users/dfbb/Sites/skycode/3rd/compress/`), all verified to exist on disk. Rust implementations take priority, Python supplements algorithm explanation. For codez-side types, read `codex-rs`.

### Group A · New Compressors

**A1. Search compressor** (grep/ripgrep: group by file, keep first/last matches, score-select segments, error weighting)
- `headroom/crates/headroom-core/src/transforms/search_compressor.rs` (902 lines, main implementation)
- `headroom/headroom/transforms/search_compressor.py` (scoring/grouping algorithm explanation)
- rtk grouping reference: `rtk/src/cmds/system/grep_cmd.rs`

**A2. Tabular compressor** (CSV/TSV/MD tables → csv-schema re-encoding; format reconstruction, does not remove content, does not attach CCR)
- `headroom/headroom/transforms/tabular_ingest.py` (table→record parsing)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/formatter.rs:205` (csv-schema formatter)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/compactor.rs` (row-by-row compaction)

### Group B · Upgrade Existing

**B1. Log → error-first + template mining (RLE)**
- `headroom/crates/headroom-core/src/transforms/log_compressor.rs` (1295 lines, level scoring/stack retention/format detection)
- `headroom/headroom/transforms/log_compressor.py` (weights ERROR=1.0/WARN=0.7/INFO=0.3/DEBUG=0.1)
- Template mining (RLE): `headroom/crates/headroom-core/src/transforms/pipeline/reformats/log_template.rs` (default min_run=3)

**B2. JSON +csv-schema +consecutive RLE dedup**
- csv-schema: `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/formatter.rs`
- Consecutive RLE dedup: `headroom/crates/headroom-core/src/transforms/smart_crusher/orchestration.rs`, `headroom/headroom/transforms/smart_crusher.py` (`dedup_identical_items`, narrowed in this design to **adjacent-only** item RLE)
- minify reference: `headroom/crates/headroom-core/src/transforms/pipeline/reformats/json_minifier.rs`

### Group C · New Techniques

**C1. base64/blob folding** (over-long base64/data-uri → placeholder)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/walker.rs` (opaque blob detection)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/compaction/classifier.rs` (cell classification: opaque)
- `headroom/crates/headroom-core/src/transforms/smart_crusher/statistics.rs` (entropy/base64 features)

**C2. Error-output protection** (error/exception and < threshold → entire segment uncompressed)
- `headroom/headroom/transforms/error_detection.py` (strong error-indicator detection)
- `headroom/headroom/transforms/content_router.py` (`protect_error_outputs` / `error_protection_max_chars=8000`)

### Group D · Introduce rtk

**D1. Generic preprocessing layer** (drop progress bars/collapse blank lines/truncate over-long lines/dedup consecutive duplicates)
- `rtk/src/core/toml_filter.rs` (8-segment pipeline main implementation, 1698 lines)
- `rtk/src/core/utils.rs` (`strip_ansi()` and other line-level utilities)
- `rtk/src/core/truncate.rs` (CAP_* capacity constants, 64 lines)
- Consecutive-duplicate dedup: `rtk/src/cmds/system/log_cmd.rs`

**D2. Command-aware routing** (call_id→command name, routing hint only)
- Command-matching mechanism: `rtk/src/core/toml_filter.rs` (`match_command` regex dispatch)
- git summarization-intent reference: `rtk/src/cmds/git/diff_cmd.rs`, `rtk/src/cmds/git/git.rs`
- No third-party reference for codez-side reverse lookup: read `FunctionCall`/`FunctionCallOutput` in `codex-rs/protocol/src/models.rs`

### Group E · CCR

- `headroom/headroom/ccr/batch_store.py` (original-text storage, main reference)
- `headroom/headroom/ccr/context_tracker.py` (thread-dimension tracking)
- `headroom/crates/headroom-core/src/ccr/mod.rs` (Rust structures, 119 lines)
- Placeholder-marker format is custom (headroom uses `<<ccr:HASH>>`, codez changes to a readable path, no direct reference)

### Shared Primitives

**S1. Scoring** (content features + last user message keyword weighting)
- `headroom/crates/headroom-core/src/transforms/anchor_selector.rs` (anchor selection/weight normalization)
- `headroom/headroom/transforms/anchor_selector.py` (query-term overlap weighting)
- Adaptive retention reference: `headroom/crates/headroom-core/src/transforms/adaptive_sizer.rs` (Kneedle)

**S2. Query extraction** (no third-party reference)
- Read `ResponsesApiRequest` from `codex-rs/codex-api` + `ResponseItem::Message` from `codex-protocol`

---

## 4. Module Interfaces and Algorithms

### 4.0 The `lossy` Definition (used throughout)

**`lossy = true` ⟺ the transformation removed substantive content (semantic definition, fix #2).** It is not judged by "byte-recoverability" — because JSON necessarily changes whitespace/key order/number representation when round-tripped through `serde_json` parse→serialize (see `zmod/llm-compress/src/compress/json.rs:30,47`), a byte-level definition would make every JSON transformation lossy, losing all discriminative meaning.

- **Lossless (lossy=false) = pure format reconstruction, all content preserved**: JSON minify, csv-schema re-encoding (object array ↔ schema+rows), table-to-JSON, consecutive blank-line normalization, consecutive duplicate-line folding, JSON consecutive duplicate-item RLE. These **remove no data**; the information the model receives is equivalent and there is nothing to retrieve → **no CCR attached**.
- **Lossy (lossy=true) = substantive content removed, the model may want to see the original**: sampling away array elements, line dropping/match dropping, head/tail truncation, over-depth subtree trimming, base64/blob folding. **CCR attached.**

> **Key constraint (fix #2/#3): compressors that produce `kind=Json` products (JSON, Tabular) perform only non-removing steps (consecutive RLE, csv-schema), so `lossy` is always false and CCR is never attached.** Thus the CCR placeholder needs only a single Text carrier, **and there is no case of injecting a CCR field into JSON**, completely avoiding the "array becomes object" / "reserved-field collision" risks. When JSON needs to remove content (depth sampling/over-depth trimming), it is **not done inside the JSON compressor**; instead detect does not claim it / yields Unchanged, handing off to Truncate for text-level handling (bare placeholder + CCR, `kind=Text`).
>
> The sole remaining JSON reserved field is RLE's `_llm_dup_prev`: **if the original array already contains an object of the `{"_llm_dup_prev":...}` form, folding of it is skipped** (kept as-is, not renamed, not enveloped), see §5①. All other placeholder markers `[llm-compress: …]` appear only in Text products.

### 4.1 router.rs: CompressOutcome adds lossy + content_kind + detect takes budget

```rust
pub enum ContentKind { Text, Json }   // identifies product form; Json is always paired with lossy=false (see §4.0)

pub enum CompressOutcome {
    Compressed { text: String, saved_bytes: usize, lossy: bool, kind: ContentKind },
    Unchanged,
}

pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str, budget: &Budget) -> bool;          // change: detect also takes budget (to read cmd)
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}
```

- `kind` (#1/#8): identifies whether the product is JSON text (JSON/Tabular compressors, always `lossy=false`) or plain text. `ccr::attach` runs only on `lossy=true` items, and `lossy=true` ⟹ `kind=Text` (§4.0 constraint), so attach **only produces bare Text placeholders**; `kind` is still kept in the interface for future extension and to assert "Json products do not attach CCR".

**Command-aware routing (#2)**: the existing router is first-match and `detect` has no cmd, so it cannot "forcibly claim". Changed to:

- `Budget` adds `cmd: Option<&CommandHint>` and `ctx: &RequestCtx`. The `detect` signature adds `budget`, letting each compressor claim based on the command name (e.g. `is_grep()` → Search.detect returns true directly).
- `ContentRouter::compress_text(text, budget)`: **first reorders the candidate compressors by `budget.cmd`** (e.g. if `is_git_diff()` hits, move Diff to the front; if `is_grep()` hits, move Search to the front), then runs first-match `detect`. With no command hint, use the default order `Json→Search→Diff→Tabular→Log→Truncate`.
- **Return type (#1)**: `compress_text` returns `Option<(String, bool, ContentKind)>` (text, lossy, kind), consistent with the §2 pseudocode.
- **Mixed fragments**: the compressor accumulates the lossy flag internally; any content-removing step sets it and writes it into `CompressOutcome.lossy`.
- Impact: the `detect` (add budget) / `Compressed{..}` (add lossy+kind) signatures and construction sites of the existing 4 compressors are updated in sync; existing test assertions are updated in sync.

### 4.2 query.rs (S2)

```rust
pub fn extract(request: &ResponsesApiRequest) -> Vec<String>
```

- From `request.input`, search **back to front** for the first `ResponseItem::Message { role:"user", .. }` and take its `InputText` text.
- Tokenization: split on non-alphanumeric → lowercase → remove stop words (built-in small table) → drop length ≤2 → keep at most N (default 32).
- No user message found → empty Vec (scoring degrades to pure content features, no error).

### 4.3 command.rs (D2)

```rust
pub struct CommandHint { pub program: String, pub argv: Vec<String> }
pub fn index(request: &ResponsesApiRequest) -> HashMap<String, CommandHint>  // key=call_id
impl CommandHint { pub fn is_git_diff(&self)->bool; pub fn is_grep(&self)->bool;
                   pub fn is_ls_like(&self)->bool; pub fn is_test_runner(&self)->bool; }
```

- Traverse the `ResponseItem::FunctionCall { name, arguments, call_id, .. }` of `request.input` (`codex-rs/protocol/src/models.rs:973`).
- **codex's actual tool names and arguments (verified, fix #1)** — the command is a **single-string command line**, not an array:
  - `shell_command`: `arguments` JSON = `{"command": "<shell string>"}` (`codex-rs/core/tests/common/responses.rs:899`).
  - `exec_command`: `arguments` JSON = `{"cmd": "<shell string>", ...}` (`codex-rs/core/src/tools/handlers/unified_exec.rs:28`).
  - After taking the string, do **lightweight shell parsing**: split on whitespace (respecting quotes), first token = `program`, the rest = `argv`; classify by program + first arg for `git diff`/`rg`/`grep`/`ls`/`pytest` etc. Parsing references `derive_exec_args` in `codex-rs/core/src/shell.rs:22`, but this module only reads, does not execute; on failure, program takes the whole string and argv is empty.
  - Others (custom function tools that are not shell): `program = name`, argv empty.
- Parse failure / non-JSON / cannot get the command field → that call_id is not entered into the index (fail-open).
- `CommandHint` serves **only as a routing hint** (§4.1): no command-specific compression logic is written; it is only used to reorder router candidates / increase aggressiveness.

### 4.4 score.rs (S1)

```rust
pub fn line_score(line: &str, query: &[String]) -> f32
```

- Content features (reference anchor_selector): contains error keywords (error/fail/panic/exception/traceback…) +1.0; contains warning (warn/warning) +0.5; +0.3 per query-word hit; blank lines/pure symbols 0.
- Purely static + query weighting, no ML. Used by Search/Log to decide which lines to keep.

### 4.5 protect.rs (C2)

```rust
pub fn should_protect(text: &str, cmd: Option<&CommandHint>, cfg: &Config) -> bool
```

- Reference error_detection.py: the text contains a strong error indicator (traceback/panic/`error:`/non-zero exit hint, etc.) **and** `text.len() < cfg.protect.error_max_bytes` (default 8192) → true (entire segment uncompressed).
- `error_max_bytes=0` → protection disabled. Command-hint assist: if cmd is a test runner and the output contains failures → increase protection inclination.
- **Highest priority (#7)**: `should_protect` is decided after ① command identification and **before** ③ preprocessing (§2 processing chain). On a hit → the entire segment is byte-for-byte unchanged, and **no preprocessing segments are performed (including ANSI/blank lines/blob/duplicate lines)**. The integrity of error output takes precedence over all compression/cleanup.

### 4.6 preprocess.rs (D1)

```rust
/// Returns (processed text, whether substantive content was removed). lossy definition see §4.0 (semantic).
pub fn run(text: &str, cfg: &PreprocessCfg) -> (String, bool)
```

Executed in order (each segment independently toggleable; if any segment errors, that segment is skipped), **with each segment marked as to whether it removes substantive content (#5)**:

1. `strip_progress` (**removes content → lossy**): drop progress bars/download lines (`^Downloading`, percentage progress, `\r` overwrite lines).
2. `blob_fold` (**removes content → lossy, #6 sole execution location**): over-long base64/data-uri segments (threshold `cfg.preprocess.blob_min_bytes`, default 256B) replaced with `[llm-compress: base64 N 字节]`. Reference walker.rs/classifier.rs. **base64/blob folding happens only here**; Truncate no longer re-folds (§5⑥).
3. `collapse_blank` (**format reconstruction → no removal**): normalize consecutive blank lines to a single blank line (only removes redundant whitespace, no substantive content, §4.0).
4. `truncate_line_bytes` (**removes content → lossy**): truncate over-long single lines by bytes (UTF-8 boundary safe), trailing placeholder; 0=disabled.
5. `dedup_consecutive` (**format reconstruction → no removal**): fold consecutive identical lines into `line content` + `[llm-compress: 上一行 ×N]`, content preserved.

- The `bool` returned by `run` = whether a **substantive-content-removing** segment (strip_progress / blob_fold / truncate_line_bytes) was triggered. If true, the orchestration layer attaches CCR to the preprocessing result (§2 processing chain).
- **Marker collision (#6 hard rule)**: before `dedup_consecutive` folds, it first scans: **if a line's original text itself matches the `^\[llm-compress: ` prefix, that line does not participate in folding** (kept as-is, RLE skipped), so the placeholder is not confused with the original. No renaming, no escaping, no enveloping — **skip on encountering the reserved prefix** is the global uniform rule (JSON's `_llm_dup_prev` works the same way, see §5①).
- The dedup here is a **shared preprocessing layer** reused by all compressors; the template mining inside the Log compressor (§5⑤) is a stronger variant.
- **Note**: even if all preprocessing segments "do not remove content", their count placeholders may still make text longer for extremely small inputs → the §2 orchestration-layer final `candidate.len() <= original.len()` gate (#4) catches and falls back, this module does not judge size.

### 4.7 ccr.rs (E)

```rust
/// Persist the fragment's original text to disk + append a Text retrieval placeholder after compressed. Called only for lossy=true items (⟹ kind=Text).
pub fn attach(compressed: String, original: &str, ctx: &RequestCtx,
              call_id: &str, cfg: &CcrCfg) -> String
```

- **Core overarching rule (#2/#6, hard-coded, cannot be weakened by implementation)**: under `cfg.enabled=true`, **lossy compression and retrievability are bound** — the result of `attach` has only two legal forms: ① successful persistence → return "lossy product + path-containing placeholder" (retrievable); ② **inability to persist for any reason** (disk failure/permissions/exceeding `max_file_bytes`/path anomaly) → **return the original text (abandon this lossy compression)**. **A third outcome of "lossy product but no retrievable path" is never allowed.** This guarantees "under `enabled=true`, anything lossy is retrievable" (§9.2 criterion 4) always holds. Only `cfg.enabled=false` allows "lossy but not retrievable" (next bullet).
- **Text products only (#2/#3)**: `attach` is called only when `lossy=true`, and `lossy=true ⟹ kind=Text` (§4.0/§4.1). So `attach` **only produces bare Text placeholders, with no JSON-injection branch**, completely avoiding array-becomes-object / reserved-field overwrite. `kind` is not passed into attach.
- **cfg gating (#5 unified semantics, only this path allows "lossy but not retrievable")**: when `cfg.enabled=false`, `attach` does not persist, **does not append a CCR retrieval path**, and returns the passed-in `compressed` directly — the compressor's **own omission placeholders** in `compressed` (e.g. `[llm-compress: 略 N 行]`) **are kept**, only without the "原文: <path>" part. That is, disabled = "has omission hint but not retrievable" (equivalent to v1). Only `enabled=true` persists and appends the path in the placeholder.
- **Granularity: one file per text fragment (#3)**: registry key = `(call_id, fragment_hash)`, `fragment_hash` = the first 12 hex of the fragment's original-text SHA256. Multiple InputTexts of the same call_id within ContentItems each persist separately, never pointing to the wrong one. `ctx.ccr` (`RefCell<CcrRegistry>`) avoids re-persisting the same fragment.
- **Path-component sanitize (#4)**: `thread_id` (= ctx.queryid) and `call_id` **are both not put directly into the path** — both come from request content and cannot be assumed to be safe filenames (may contain `/`, `..`, be over-long). Rule: `thread_dir = sanitize(queryid)` (non-`[A-Za-z0-9_-]` characters replaced with `_`; if over 64 bytes, take the first 16 hex of its SHA256); filename = `<sanitize(call_id truncated to 32)>-<fragment_hash>.txt`. Final path `~/.codex/llm-compress/ccr/<thread_dir>/<filename>`, guaranteeing no path traversal and no over-length.
- **Cleanup dual limits (#5)**: before writing, for that thread directory: ① if the file count exceeds `cfg.max_files_per_thread` (default 200) → delete oldest by mtime; ② if the directory's total bytes exceed `cfg.max_thread_bytes` (default 64 MiB) → continue deleting oldest by mtime until within limit.
- **Single-file over-limit = abandon compression (#6, preserve "anything lossy is retrievable")**: when `enabled=true` and the original exceeds `cfg.max_file_bytes` (default 4 MiB), **no non-retrievable lossy compression is produced** — `attach` returns the **original text** (abandon this lossy compression), rather than a persist-failure-style "lossy placeholder with no path". This keeps the invariant "under `enabled=true`, any lossy item has a retrievable CCR file" (§9.2 criterion 4) always true.
- **Second size check**: after `attach` assembles the placeholder, if `attached.len() > original.len()`, downgrade to a shorter reference (`[llm-compress: 略,见 ccr/<fragment_hash>]`); if still over, abandon and return the original text. This is a local check inside attach; the orchestration layer (§2) also has a final unified gate (#4), and the two stack.
- **fail-open**: persistence failure (disk full/permissions/scan failure) only `tracing::warn`, then **abandon this lossy compression and return the original text** (same policy as #6: leave no non-retrievable lossy product). Note: this is the fallback under `enabled=true`; `enabled=false` is a separate path (cfg gating above), and when disabled there is no retrievability commitment in the first place, so keeping the compressor's omission placeholder is sufficient.

### 4.8 compress/schema.rs (csv-schema shared module)

```rust
/// Object array (items homogeneous) → {"_schema":[...],"_rows":[[...]]}. Non-homogeneous → None.
/// Pure format reconstruction, all content preserved → lossy=false, no CCR attached (§4.0).
pub fn to_schema_form(value: &Value) -> Option<Value>
```

- Homogeneity decision: all elements are objects with identical key sets (reference the csv-schema applicability conditions of formatter.rs).
- Scalar values go into `_rows`; the values of nested objects/arrays are kept as-is in the row (no forced flattening, to avoid breakage).
- The product is still valid JSON (preserving the hard invariant). The JSON compressor calls it on `Value::Array` (§5① step 2); Tabular first parses the table into `Value::Array<Object>` then calls it (§5④).
- **All content preserved** (key names moved to `_schema`, values moved to `_rows`, no data removed), so per the §4.0 semantic definition it is marked **lossy=false, no CCR attached**. The model can read all data from `_schema`+`_rows`.

---

## 5. Six-Compressor Algorithms

Routing order `Json → Search → Diff → Tabular → Log → Truncate`. All `compress` run inside the router's `catch_unwind`; on failure, fall back to the original text.

### ① JSON (upgrade json.rs) — only non-removing steps, never attaches CCR

The product is JSON text, `kind=Json`, **`lossy` always false** (§4.0/§4.1 constraint). The JSON compressor **does only the two non-removing steps**:

1. **Consecutive duplicate-item RLE dedup (no removal)**: fold only adjacent array items whose `Value` is fully equal, folding into: keep the first item + immediately following placeholder `{"_llm_dup_prev": N}` (semantics = the previous item repeats N more times, N+1 items in total including the first). All data preserved → **lossy=false, no CCR attached**. Consecutive RLE only, no non-consecutive dedup (which would lose order/position). Reference `smart_crusher/orchestration.rs` (narrowed to consecutive RLE).
   - **Reserved-field collision (#3 unified rule)**: if the original array already contains an object of the `{"_llm_dup_prev":...}` form, **folding of it is skipped** (kept as-is), from the same root as the "skip on encountering reserved prefix" of §4.6 dedup — no renaming, no escaping, no enveloping.
2. **csv-schema in-band representation (no removal)**: when object-array items are homogeneous, call `schema::to_schema_form` to rewrite into `{"_schema":[...],"_rows":[...]}`. All data preserved → **lossy=false, no CCR attached** (§4.8). Disable via `cfg.json.csv_schema=false`.

- **detect decides the hand-off (fix #1/#4, adapting to router first-match)**: the router is first-match — once detect hits, even if compress returns Unchanged, later compressors are not tried (`zmod/llm-compress/src/router.rs:36`). So JSON **cannot** rely on "compress returns Unchanged to let Truncate take over". Instead, **predict inside `JsonCompressor::detect(text, budget)` after parsing**, with the criterion "**whether the size after lossless compression (RLE/csv-schema) still exceeds `cfg.truncate.max_bytes`**" — claim only when the lossless steps suffice to bring the size within budget, otherwise yield to Truncate as the fallback:
  - Parsable as JSON, and the **estimated** post-lossless-compression size `≤ cfg.truncate.max_bytes` (lossless is enough, no lossy fallback needed) → detect returns **true**, the JSON compressor handles it (producing `kind=Json,lossy=false`). This covers two cases: not exceeding the threshold to begin with (small JSON, possibly Unchanged kept as-is), or RLE/csv-schema gains compressing it within the threshold.
  - Parsable, but the **estimated** post-lossless-compression size is **still > `cfg.truncate.max_bytes`** (lossless gains insufficient to meet the budget, regardless of any minor gains) → detect returns **false**, yielding to the subsequent Truncate for text-level sampling/truncation (bare placeholder + CCR, `kind=Text`). An array exceeding `max_array_items` / nesting exceeding `max_depth` that lossless cannot compress down falls into this branch.
  - Not parsable → detect returns false (not claimed in the first place).
  - **Estimation method**: detect may actually run the lossless steps (RLE+csv-schema) to get the product length and compare against `truncate.max_bytes` (parse once, reuse for compress, avoiding repeated parsing); or use a conservative estimate. Pick one at implementation time, with "whether it exceeds the threshold after lossless" as the deciding factor.
- **Do no content-removing step inside the JSON compressor**: v1's "long-array sampling / over-depth truncation" is removed. `cfg.json.max_array_items`/`max_depth` together with `truncate.max_bytes` are used in the **detect phase** to decide the hand-off (the core threshold is `truncate.max_bytes`), not removing elements inside compress.
- The product must be re-parse-validated via `serde_json`; on failure fall back to the original (existing invariant). RLE/csv-schema products are inherently valid JSON.

### ② Search (new search.rs) — drops matches, attaches CCR

- **detect(&self, text, budget)**: multi-line and most lines match `path:line_no:content` or `path:line_no:col:content` (ripgrep); when `budget.cmd.is_grep()` is true, claim directly (detect can now read budget, see §4.1).
- **compress** (reference search_compressor.rs): group by file path; within each group, every file must keep first+last matches, the middle selected as top-K by `score::line_score` (query-weighted) (`cfg.search.max_per_file`, default 5), restored to sort order; if the file count exceeds `cfg.search.max_files` (default 15) → keep high-scoring files by group total score, fold the rest into `[llm-compress: 略 N 个文件]`; the placeholder notes the number of omitted matches/files.
- `lossy=true` (matches dropped), `kind=Text`, attaches CCR.

### ③ Diff (diff.rs unchanged) — drops context, attaches CCR via orchestration

- Algorithm stays as v1 (preserve all changed lines + fold surplus context). Changes: `detect` adds the `budget` parameter; `Compressed{..}` adds `lossy=true` (when folding occurs) and `kind=Text`, and the orchestration layer attaches CCR accordingly.
- When `budget.cmd.is_git_diff()` is true, the router moves Diff to the front of the candidates (§4.1 reordering).

### ④ Tabular (new tabular.rs) — format reconstruction, no removal, no CCR

- **detect(&self, text, budget)**: does not claim when `cfg.tabular.enabled=false`. Returns true only if it is CSV/TSV (multi-line, stable delimiter) or a Markdown table (`|---|` separator row), **and all of the following strict preconditions are met**; **if any is unmet, detect returns false** (yielding to the subsequent Log/Truncate — because the router is first-match, the degradation decision must be inside detect, it cannot rely on compress returning Unchanged, #1):
  1. There is a clear header row (CSV first row / Markdown header); no header → false.
  2. Column names are **unique and non-empty**; duplicate or empty column names → false (avoid object-key overwrite, column loss).
  3. All data rows have the same column count as the header; uneven column count → false.
  4. No escaped delimiters, no in-cell line breaks (a simple parser cannot stably restore them) → false on hit.
- **compress** (reference tabular_ingest.py): detect already guarantees the preconditions are met, so parse into a record array → call `schema::to_schema_form` → output valid JSON `{"_schema",_rows}`, `kind=Json`. (Defensively: if parsing unexpectedly fails, still return `Unchanged`.)
- **lossy=false**: all row/column data enters `_schema`+`_rows`, all data preserved → no CCR attached (§4.0). The original delimiter/alignment presentation is not preserved, but this is not substantive-content removal.

### ⑤ Log (rewrite log.rs) — template folding (no removal) + score-based line dropping

- **Template mining (RLE, no removal) first**: consecutive same-template lines (differing only in variables) → template head + variable table (all variables preserved). Reference log_template.rs (`cfg.log.template_min_run`, default 3). lossy=false.
  - **Marker collision (#6)**: the template head/variable table uses a fixed prefix (e.g. `[llm-compress: 模板]`); if the original already contains lines with the same prefix, escape at implementation time without affecting the lossy decision (format reconstruction).
- **Level-score retention (line dropping)**: the remaining lines use `score::line_score`, with levels corresponding to `cfg.log.keep_levels` (default `["error","warn"]`) + stack frames mandatorily kept, surplus DEBUG/INFO dropped by score; keep first/last + high-scoring middle errors. Reference log_compressor.rs.
- Line dropping occurs → `lossy=true`, attaches CCR (`kind=Text`); pure template folding (no removal) is `lossy=false`.
- Replaces v1's "positional head/tail truncation", solving the "middle ERROR/stack frames folded indiscriminately" problem.

### ⑥ Truncate (change truncate.rs) — drops content, attaches CCR

- Fallback: strip_ansi (existing) + head/tail + hard truncation over max_bytes (UTF-8 safe, existing).
- **base64/blob folding is not here**: moved up to the preprocessing-layer `blob_fold` segment (§4.6, #6 sole location); the text Truncate receives has already had its blobs folded, so no re-processing.
- `lossy=true` (truncation removes content), `kind=Text`, attaches CCR.

---

## 6. Full Configuration Set

Incremental on v1 fields (all existing fields preserved). Whole section missing or `enabled=false` → fully off, zero changes; any sub-table missing → use defaults. All new fields go into `config.rs`'s `Default`, following `#[serde(default)]`.

```toml
[llm_compress]
enabled = false
min_total_bytes = 4096
per_item_min_bytes = 1024

# ── Existing (preserved) ──
[llm_compress.truncate]
head_lines = 50
tail_lines = 50
max_bytes = 16384

[llm_compress.json]
max_array_items = 20           # participates in the detect-phase decision: if post-lossless-compression size still > truncate.max_bytes, detect false hands off to Truncate (arrays over this length often cannot be compressed down)
max_depth = 6                  # same as above in detect phase; the final hand-off criterion is uniformly "whether post-lossless-compression size still exceeds truncate.max_bytes" (§5①)
csv_schema = true              # convert object arrays to csv-schema (format reconstruction, no removal, no CCR)
# Note: the JSON compressor does only non-removing steps (RLE/csv-schema); when content removal is needed, detect hands off to Truncate (§5①), no element removal inside JSON

[llm_compress.diff]
context_lines = 3

[llm_compress.log]
dedup_repeats = true           # existing
template_min_run = 3           # new: minimum consecutive lines for RLE template mining
keep_levels = ["error", "warn"]  # new: levels to mandatorily keep

# ── New sub-tables ──
[llm_compress.preprocess]      # D1
strip_progress = true
collapse_blank = true
truncate_line_bytes = 2000     # 0=disabled
dedup_consecutive = true
blob_min_bytes = 256           # base64/data-uri folding threshold

[llm_compress.search]          # A1
max_per_file = 5
max_files = 15

[llm_compress.tabular]         # A2
enabled = true

[llm_compress.protect]         # C2
error_max_bytes = 8192         # 0=disable protection

[llm_compress.ccr]             # E
enabled = true
max_files_per_thread = 200     # per-thread directory file-count limit (LRU)
max_thread_bytes = 67108864    # per-thread directory total-byte limit 64 MiB (LRU deletes until within limit)
max_file_bytes = 4194304       # single CCR file limit 4 MiB; under enabled, original exceeding this → abandon compression and return original (preserve "anything lossy is retrievable")
```

---

## 7. Error Handling (fail-open throughout)

Inherits all of v1's fail-open, with failure handling for the new modules:

- `query.rs`/`command.rs` parse failure → return empty / skip that item, no error.
- `preprocess.rs` any segment errors → that segment is skipped, returns the previous segment's result.
- `protect.rs` decision error → treat as not protected (continue compressing, conservative).
- `ccr.rs` persistence failure (or original exceeds `max_file_bytes`) → only warn, then **abandon this lossy compression and return the original text** (leave no non-retrievable lossy product, §4.7); does not block the request. `ccr.enabled=false` is a separate path: keep the compressor's own omission placeholder, do not append a retrieval path.
- `schema.rs` homogeneity decision fails → return None, JSON/Tabular go to their respective fallbacks.
- All compressors are still inside the router's `catch_unwind`.
- `transform` still returns `()`, type-level guaranteeing compression failure cannot block the request.

---

## 8. Test Data Inheritance

### 8.1 Sources and Licensing

Both `headroom` and `rtk` are **Apache-2.0**, allowing copying with copyright retained. Inherited data is placed at:

```
zmod/llm-compress/tests/fixtures/inherited/
  LICENSE-headroom   # Apache-2.0 + © 2025 Headroom Contributors
  LICENSE-rtk        # Apache-2.0 + © 2024 rtk-ai Labs
  NOTICE.md          # per-file registry: this repo's relative path → adapted from which upstream path + license
  manifest.toml      # sidecar: each fixture's source, corresponding compressor, ref_output path, which invariant is tested
  search/ log/ diff/ json/ tabular/ preprocess/   # categorized by compressor, containing pure raw data files
```

- **Write no source/copyright comment inside any fixture file (#8)**: inherited data contains JSON and real command output; adding `//` to JSON would make it invalid, and adding a header to `.txt` would pollute the input and affect compression/parity results. Source and copyright are recorded **only** in `NOTICE.md` + `manifest.toml` (sidecar); fixture files keep upstream raw bytes unchanged.
- Each `manifest.toml` entry: `{ file = "search/grep_basic.txt", origin = "headroom/.../search_compressor.rs:680", compressor = "search", ref_output = "search/grep_basic.expected", invariants = ["关键行保留","体积不劣"] }`. The test loads the manifest to locate input/ref_output, without relying on in-file comments.

### 8.2 Inheritance List (exact sources)

| Our compressor | Inherited input source | expected comparison source |
|---|---|---|
| Search | `headroom/crates/headroom-core/src/transforms/search_compressor.rs:668-900` (16 embedded examples: standard grep, ripgrep context, Windows paths, filenames with `-`, content containing `:`) | same place + `headroom/tests/test_search_compressor.py` (49 examples) |
| Log | `headroom/tests/parity/fixtures/log_compressor/*.json` (20, including a real log of 305→8 lines) | `output.compressed` of the same JSON |
| Diff | `headroom/tests/parity/fixtures/diff_compressor/*.json` (27) | `output` of the same JSON |
| JSON | `headroom/tests/parity/fixtures/smart_crusher/*.json` (17: 30/100-item arrays, nesting, duplicates, unicode, empty, passthrough) | `output` of the same JSON |
| Preprocessing | `rtk/src/core/toml_filter.rs:712-1000`, `rtk/src/core/utils.rs:401-859` | expected at the same place |
| Command-rule reference | the `[[tests]]` in `rtk/src/filters/*.toml` (gcc/gradle/basedpyright etc. ~150 examples) | expected at the same place |
| Real command output | `rtk/tests/fixtures/*.txt` (42: Maven/Gradle/GitLab CI/.NET) | — |
| Tabular | no dedicated fixture → reverse-construct CSV/MD from smart_crusher object arrays + a small amount self-made | self-made, manually verified |
| Error protection | `headroom/tests/parity/fixtures/content_detector/*.json` (21, including error-log recognition) | — |

### 8.3 Comparison-Assertion Form (key)

Our algorithm is thin, the placeholder marker differs (`[llm-compress: …]` vs `<<ccr:HASH>>`), and defaults differ, so **no byte-for-byte equality**. For each inherited `(input, ref_output)`, assert a set of invariants:

1. **Size not worse than reference**: `our_output.len() ≤ ref_output.len() * 1.5` (thin allows being slightly looser, but not absurdly so).
2. **All key lines preserved**: the "error lines/changed lines/first-and-last matches" preserved by the reference output must also be contained in ours (compared using the high-score line set from `score::line_score`).
3. **Our hard invariants**: compressed ≤ pre-compression, valid UTF-8, JSON products parseable, placeholder marker present.
4. **CCR reversibility (#9, asserted only under `ccr.enabled=true`)**: the lossy item's CCR file is persisted and its content == that fragment's original text (note: CCR stores the **fragment's original text**, not the whole input; one file per fragment, §4.7). `parity_test` and all CCR-reversibility tests **run fixed under the `ccr.enabled=true` config**; `ccr.enabled=false` is a separate case asserting "lossy compression occurs as usual, no CCR file, the placeholder **keeps the compressor's own omission hint but contains no retrieval path** (§4.7 #5)", and does **not** assert retrievability. Also: for the case where `enabled=true` and the original exceeds `max_file_bytes`, assert that **the item falls back to the original (uncompressed)** rather than producing a non-retrievable lossy product (§4.7 #6).

Our expected is still produced by the implementation + manually verified and frozen (insta snapshot); the reference output is used only for the comparison assertions above, not asserted equal directly.

---

## 9. Test Organization and Success Criteria

### 9.1 Test Organization

```
tests/
  fixtures/inherited/...                          # inherited input + ref_output + LICENSE/NOTICE
  search_test.rs  tabular_test.rs                 # new compressors
  json_test.rs (extended)  log_test.rs (rewritten)  # upgrades
  preprocess_test.rs  command_test.rs  query_test.rs
  score_test.rs  protect_test.rs  ccr_test.rs  schema_test.rs
  parity_test.rs                                  # traverse inherited/ and run the 8.3 comparison assertions
  snapshots/                                      # insta freezing of our expected
```

- Unit tests for each new module: query (extraction / no user), command (parse `shell_command.command`/`exec_command.cmd` string → program/argv, skip non-JSON, is_* classification), score (error lines high score / query weighting), protect (error and small → protect, large → not protect), preprocess (each segment independent + combined, content-removing segments [strip_progress/blob_fold/truncate_line] return lossy=true, format-reconstruction segments lossy=false, skip folding on lines with the `[llm-compress:` prefix), schema (homogeneous → rewrite, non-homogeneous → None, nested values preserved), ccr (only produces Text placeholders, path-component sanitize [including `/`, `..`, over-long], one file per fragment key=(call_id,fragment_hash), file-count/total-byte dual-limit LRU, **under enabled, single file over max_file_bytes → return original**, **under enabled, persistence failure → return original / abandon this lossy compression**, cfg.enabled=false → no persistence and keep compressor omission placeholder, second size check downgrade/abandon).
- New compressors: search (grouping / keep first-last / over-file folding / query weighting / `is_grep` hit detect, lossy=true attaches CCR), tabular (strict preconditions met → schema product parseable, lossy=false no CCR; **no header / duplicate column names / empty column names / uneven columns / escaped delimiters / in-cell line breaks → detect returns false, Truncate hits** [#1]).
- router: detect takes budget; `compress_text` returns `Option<(String,bool,ContentKind)>`; on `is_git_diff`/`is_grep` hit, candidates are reordered to the front.
- Upgrade regression: json (consecutive RLE dedup no removal + csv-schema no removal + product valid JSON + `kind=Json` always `lossy=false` no CCR + **detect hand-off uses "whether it still exceeds `truncate.max_bytes` after lossless compression" as criterion: ≤ threshold after lossless → claim (including small JSON as-is / gains compressing within threshold); still > threshold after lossless (gains insufficient, including huge JSON with only minor lossless gains, arrays over max_array_items / nesting over max_depth not compressible down) → detect returns false, Truncate hits** [#1]), log (template folding no removal + level-score line dropping attaches CCR + middle ERROR not lost).
- lossy and kind invariants: `kind=Json ⟹ lossy=false` (JSON/Tabular never attach CCR); `lossy=true ⟹ kind=Text` (attach only produces Text placeholders). RLE/csv-schema/blank-line normalization/consecutive folding lossy=false; match dropping/line dropping/truncation/blob/strip_progress lossy=true.
- Orchestration: content-removing preprocessing segments also attach CCR, pure format-reconstruction preprocessing does not attach but keeps the result, **protection-gate hit keeps the entire segment byte-for-byte unchanged (including ANSI/blank lines/blob/duplicate lines, none processed, #7)**, final write-back uniformly passes the candidate≤original gate (#4), small-input count placeholders growing longer fall back to original.
- Invariants (preserved + #3/#4/#5): `enabled=false` byte-for-byte unchanged, **compressed ≤ pre-compression (two gates: inside attach + orchestration-layer final)**, UTF-8 safe, JSON products parseable (JSON compressor has no CCR injection, products inherently valid), only touches the two variants, ContentItems images untouched, CCR dual limits do not exceed disk.
- ccr/persistence tests use `tempfile` + injected HOME (following the stats_test pattern). dev-deps `insta`/`tempfile` already present.
- Development uses the symlink member: `cd codex-rs && cargo test -p codez-llm-compress`.

### 9.2 Success Criteria

1. The six compressors (Json/Search/Diff/Tabular/Log/Truncate) + preprocessing + protection gate + CCR all land, the routing priority `Json→Search→Diff→Tabular→Log→Truncate` takes effect; command hints can reorder candidates (detect takes budget).
2. The `parity_test` of inherited fixtures is fully green (the four invariant categories of 8.3).
3. Hard invariants satisfied: **compressed ≤ pre-compression (second check after CCR placeholder concatenation, downgrade/abandon if over, #3)**, valid UTF-8, JSON products parseable, only touches the two variants, image/encrypted content untouched, `enabled=false` byte-for-byte unchanged.
4. CCR (#9): when `ccr.enabled=true`, lossy compression (line dropping/match dropping/truncation/blob) **must** persist the fragment's original text and be retrievable, with the placeholder being a **bare Text marker** containing the path (JSON/Tabular products are `lossy=false` and do not attach CCR); one file per fragment (registry key=(call_id,fragment_hash)); path components sanitized; file-count/total-byte dual limits; **original exceeds max_file_bytes or persistence fails → the item falls back to original (no non-retrievable lossy product, #6)**. When `ccr.enabled=false`: lossy compression occurs as usual but is not persisted, the placeholder keeps the compressor's own omission hint but contains no retrieval path, and retrievability is **not required**. Parity and reversibility tests run fixed under enabled.
5. **transform signature and integration-point patch unchanged**: command context (`shell_command`/`exec_command` string parsing) / query keywords are all extracted from request, with no new core touchpoints, and the tool system is untouched.
6. fail-open fully covered: any new-module failure falls back to the original / skips, without blocking the request.

---

## 10. Suggested Implementation Order

Dependency relationships (shared primitives first):

```
1 router (CompressOutcome adds lossy+kind:ContentKind + detect takes budget + compress_text returns the triple + candidate reordering) + schema.rs ─┬─> 4 JSON upgrade
2 query + command (shell_command/exec_command parsing) + score  ├─> 5 Search
3 preprocess (content-removing/format-reconstruction classification + blob_fold) + protect      ├─> 6 Tabular
                                                            ├─> 7 Log rewrite
                                                            ├─> 8 Truncate (without blob)
9 ccr (cfg gating + registry + sanitize + dual limits + second size check) ─┴─> 10 lib orchestration wiring
                                                              11 inherited fixtures + parity_test
```

- 1 is the interface foundation: `CompressOutcome` adds `lossy` **and `kind: ContentKind` (#8)** + `detect(&self,text,budget)` + `compress_text` returns `Option<(String,bool,ContentKind)>` + router candidate reordering — **change it first so that compressors 4-8 have a unified signature (each must add the detect budget parameter and the two fields lossy+kind on `Compressed{..}`)**. schema.rs is also part of the foundation (shared by JSON/Tabular).
- 2-3 are shared modules with no dependency, parallelizable with 1. blob_fold belongs to preprocess (#6 sole location).
- 4-8 each compressor depends on 1-3, mutually independent and parallelizable; all need `detect` changed to take budget and `Compressed{..}` augmented with lossy+kind. JSON/Tabular are always `kind=Json,lossy=false`; Truncate no longer does blob (moved up).
- 9 ccr depends on RequestCtx's mutable registry (#8) and CcrCfg (#4/#5), produces only Text placeholders, includes path sanitize, dual-limit cleanup, second size check.
- 10 orchestration wires everything (including content-removing preprocessing segments attaching CCR + the final candidate≤original gate). 11 data inheritance (NOTICE+manifest sidecar) and comparison tests wrap up.
