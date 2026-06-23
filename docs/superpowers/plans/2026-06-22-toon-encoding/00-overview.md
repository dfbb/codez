# TOON Encoding Implementation Plan — Overview

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **One task per file.** Each `task-N-*.md` in this directory is a self-contained
> task with its own header context. Implement them in numeric order. An
> implementer reads only their own task file plus this overview.

**Goal:** Re-encode JSON tool-output content as TOON (Token-Oriented Object
Notation) to cut token usage, while the wire envelope stays JSON and the model
still emits JSON as always.

**Architecture:** `JsonCompressor` and `TabularCompressor` parse their input to a
`serde_json::Value`, encode it to TOON via the `toon-format` crate, and verify a
round-trip (`decode_default(&toon) == value`) before claiming. TOON products use
a new `ContentKind::Toon` that the orchestrator treats like `Json` — never
decorated with a CCR pointer. The homemade `_schema`/`_rows` form (`schema.rs`)
and the RLE dedup step are removed; TOON's tabular form replaces them.

**Tech Stack:** Rust 1.95.0, `toon-format = "0.5"`, `serde_json`, the existing
`codez-llm-compress` crate (`zmod/llm-compress`).

**Source spec:** `docs/superpowers/specs/2026-06-22-toon-encoding-design.md`

## Global Constraints

Every task's requirements implicitly include this section.

- **Toolchain:** Rust 1.95.0 (workspace `rust-toolchain.toml`). The dep must
  compile on it.
- **Dependency line (exact):** `toon-format = { version = "0.5", default-features = false }`
  — `default-features = false` is REQUIRED; the crate's default `cli` feature
  pulls in clap/tiktoken-rs/ratatui/comfy-table and must NOT be enabled.
- **`toon-format` API used:** `toon_format::encode_default(&value) -> Result<String, ToonError>`
  and `toon_format::decode_default::<Value>(&toon) -> Result<Value, ToonError>`.
  Both are always available (not feature-gated).
- **Round-trip self-check is mandatory** for every TOON product: encode, then
  `decode_default::<Value>` and compare to the original `Value`; any error or
  inequality → fall back (return `Unchanged` / `None`). TOON is the model's only
  view of that tool output.
- **Five claim conditions** (both compressors, identical): (1) `json.use_toon ==
  true`; (2) input parses to a `Value`; (3) `encode_default` succeeds AND
  round-trip passes; (4) `toon.len() < original.len()` (strictly smaller);
  (5) `toon.len() <= truncate.max_bytes`. `detect` and `compress` share one
  helper so they never disagree.
- **`ContentKind::Toon` is `lossy == false` always** and never gets a CCR pointer.
- **fail-open:** never panic, never block a request; on any doubt return the
  original text.
- **Build & test:** run inside `codex-rs/` with the dev symlink in place
  (`codex-rs/llm-switch`-style; here `codex-rs/llm-compress -> ../zmod/llm-compress`
  + a `"llm-compress"` member line in `codex-rs/Cargo.toml`, both uncommitted).
  Test command: `cargo nextest run -p codez-llm-compress`. Build:
  `cargo build -p codez-llm-compress`.
- **Commits:** stage only `zmod/llm-compress/...` paths. NEVER `git add -A`/`.`/
  `codex-rs`. The `codex-rs/Cargo.toml` member line and `codex-rs/Cargo.lock`
  stay uncommitted dev scaffolding.
- **Comments and docs in English.**

## Task list (numeric order)

1. `task-1-dep-contentkind-toon-helper.md` — add `toon-format` dep, add
   `ContentKind::Toon`, add shared `compress/toon.rs` encode+round-trip helper.
2. `task-2-config-use-toon.md` — add `use_toon` to `JsonCfg` (default true),
   additive (csv_schema / TabularCfg untouched here).
3. `task-3-jsoncompressor-toon.md` — rewrite `JsonCompressor` to emit TOON; drop
   RLE; fix `parity_test` JSON/TOON assertion.
4. `task-4-tabularcompressor-toon.md` — rewrite `TabularCompressor` to emit TOON;
   change orchestrator to skip CCR for `ContentKind::Toon`; adapt the end-to-end
   CCR-isolation test.
5. `task-5-cleanup-dead-code.md` — delete `schema.rs` + `schema_test.rs`, remove
   `csv_schema` field and `TabularCfg`, fix `config_test`.

## Dependency note (verify in Task 1)

`toon-format` pulls `serde_json` with the `preserve_order` feature. Cargo feature
unification then enables `preserve_order` for the whole workspace (Map iteration
follows insertion order everywhere). This is deterministic and aligns with our
cache-stability goal, but Task 1 must run the full `codez-llm-compress` suite to
confirm nothing in our crate depended on the old sorted-key order.
