# codez-llm-compress

Compress tool call return contents in place at the LLM request sending boundary in codex, reducing token consumption.

> **TOON Encoding**: JSON objects/arrays and table-like tool outputs are encoded by default as [TOON](https://github.com/toon-format/toon-rust) (Token-Oriented Object Notation), which is more token-efficient than JSON. The online request envelope remains JSON, and **models output JSON as usual**—only the string content the model *reads* changes. Controlled uniformly via `[llm_compress.json] use_toon` (defaults to true); a `decode → original value` round-trip self-check is performed before writing back, reverting to the original if irreversible (fail-open).

## Architecture Overview (v2)

```
transform(request)
  ├─ config::load()            — Read ~/.codex/config-zmod.toml [llm_compress]
  ├─ ccr::RequestCtx           — Construct one-time request context
  │    └─ command::index()     — Build call_id → command name index
  ├─ build_router()            — Json→Search→Diff→Tabular→Log→Truncate
  └─ per-item compress_in_place()
       ① per_item_min_bytes threshold gate
       ② protect::should_protect()  — Protection gate (skip original if hit)
       ③ preprocess::run()          — ANSI cleanup / progress line removal
       ④ ContentRouter::compress_text() — Route compression
       ⑤ ccr::attach()              — Write CCR archive for lossy
       ⑥ Volume gate (only write back ≤ original)
```

## Six Compressors

| Priority | Compressor | Activation Condition | Method |
|----------|------------|----------------------|--------|
| 1 | JsonCompressor | `use_toon=true`, valid JSON object/array, TOON output strictly smaller and ≤ truncate.max_bytes | Encode as TOON (lossless, kind=Toon, no CCR; round-trip self-check before write-back) |
| 2 | SearchCompressor | grep/rg-style results | Group by file, preserve first/last match + score-based segment selection (lossy, attach CCR) |
| 3 | DiffCompressor | git diff / unified diff | Collapse large context sections (lossy, attach CCR) |
| 4 | TabularCompressor | `use_toon=true`, CSV/TSV/Markdown tables, TOON output strictly smaller and ≤ truncate.max_bytes | Encode as TOON (lossless, kind=Toon, no CCR; round-trip self-check before write-back) |
| 5 | LogCompressor | Log lines (with timestamps/stack frames/duplicates) | Level-scored retention (preserve error/warn/stack frames, delete low-value lines; lossy, attach CCR) |
| 6 | TruncateCompressor | Arbitrary text (fallback) | Keep head/tail + truncate middle to max_bytes (lossy, attach CCR) |

## Preprocessing Layer

`preprocess::run()` performs lossless or lossy cleanup on the raw string before routing compression:

- ANSI escape sequence stripping (lossless)
- Progress bar line removal (lossy, counted in CCR)
- Can be toggled per-need via `[llm_compress.preprocess]` config options

## Command Awareness

`command::index()` extracts call_id → command name mappings from the tool call history in the request, with `Budget.cmd` passing the command name to each compressor. Compressors can adjust compression strategy accordingly (e.g., log compressor retains more error lines for `run_tests` calls).

## CCR Retrieval Mechanism

After lossy compression, `ccr::attach()` writes the original content to `~/.codex/llm-compress/ccr/<queryid>/` directory and inserts a retrieval hint at the head of the compressed result. Users or subsequent codex tools can retrieve the original content as needed.

CCR can be disabled via `[llm_compress.ccr].enabled = false`.

## Configuration

`~/.codex/config-zmod.toml`:

```toml
[llm_compress]
enabled = true
per_item_min_bytes = 512   # Items below this byte count are skipped

[llm_compress.truncate]
head_lines = 50            # Head lines to preserve on truncation
tail_lines = 50            # Tail lines to preserve on truncation
max_bytes = 65536

[llm_compress.json]
use_toon = true            # JSON objects/arrays + tables uniformly encoded as TOON (defaults to true);
                           # gates both JsonCompressor and TabularCompressor. Set false to disable both

[llm_compress.diff]
context_lines = 3          # Context lines to preserve per hunk

[llm_compress.search]
max_per_file = 5           # Max match lines to keep per file
max_files = 15             # Max files to keep, collapse if exceeded

[llm_compress.log]
keep_levels = ["error", "warn"]  # Log levels to always keep

[llm_compress.preprocess]
strip_progress = true       # Remove progress/download lines
collapse_blank = true       # Collapse consecutive blank lines to one
truncate_line_bytes = 2000  # Truncate oversized single lines by bytes (0=disabled)
dedup_consecutive = true    # Collapse consecutive duplicate lines to count
blob_min_bytes = 256        # Fold base64/blob lines exceeding this length (0=disabled)

[llm_compress.ccr]
enabled = true
max_files_per_thread = 200      # Max files per directory per thread
max_thread_bytes = 67108864     # Max total bytes per directory per thread (64 MiB)
max_file_bytes = 4194304        # Max single file size (4 MiB), abandon compression if exceeded

[llm_compress.protect]
error_max_bytes = 8192      # Outputs with error marker and smaller than this byte count are not compressed (0=protection disabled)
```

When the file or table is missing, the corresponding feature is **disabled** by default (fail-safe).

## Build

```bash
# From codez-v2 root
cd codex-rs
cargo build -p codez-llm-compress

# Full test suite (isolate HOME to avoid local config-zmod interference)
CARGO_HOME=/Users/dfbb/.cargo HOME=$(mktemp -d) cargo test -p codez-llm-compress
```

## Test Structure

| File | Coverage |
|------|----------|
| `tests/transform_test.rs` | Black-box regression (image preservation, etc.) |
| `tests/orchestration_test.rs` | End-to-end orchestration chain |
| `tests/parity_test.rs` | Inherited fixture invariants (no size regression, TOON decodable round-trip) |
| `tests/*_test.rs` | Per-module unit tests |

See `tests/fixtures/inherited/NOTICE.md` for inherited fixture sources.
