# llm-compress v2 Implementation Plan — Master Index

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to execute task by task. Each task is a standalone plan file, with steps tracked via `- [ ]` checkboxes.

**Goal:** Extend the compression capabilities of the already-shipped v1 `codez-llm-compress`: add Search/Tabular compressors, upgrade Log/JSON, introduce rtk generic preprocessing + command-aware routing, error-output protection, and CCR (original-content retrieval).

**Architecture:** Reuse v1's `ContentRouter` + `Compressor` trait + fail-open. Inside `transform`, first build a one-shot `RequestCtx` (query keywords + command index + mutable CCR registry), then run each segment through the chain "command identification → protection gate → preprocessing → routed compression → CCR attachment → size gate". The transform signature and the integration-point patch are left unchanged.

**Tech Stack:** Rust (edition 2021), serde / serde_json, toml, chrono, tracing, sha2; dev-deps `insta`, `tempfile`. Reverse path dependencies on `codex-api` / `codex-protocol`.

**Design basis:** `docs/superpowers/specs/2026-06-21-llm-compress-v2-design.md` (finalized, after six review rounds). Each task is annotated with the spec subsections it covers.

---

## Global Constraints

Copied verbatim from spec §1; every task implicitly includes this section:

- **Do not change the transform signature**: `pub fn transform(request: &mut ResponsesApiRequest, _api_provider: &ApiProvider, queryid: &str)`, returning `()`. All input for new capabilities is extracted from `request`.
- **Do not change the integration-point patch**: no new touchpoints in `core/src/client.rs`, no changes to the codex tool system.
- **Handle only two variants**: the `output` of `FunctionCallOutput` / `CustomToolCallOutput`. For `Text(s)`, compress `s`; for `ContentItems`, compress only `InputText.text`, leaving images/encrypted content unread and unchanged — never flatten.
- **fail-open throughout**: on any failure, fall back to the original content / skip, never blocking the request; non-test code does not use `unwrap`/`expect` (except at `catch_unwind` boundaries).
- **Unified placeholder marker** `[llm-compress: …]`. **Post-compression size ≤ pre-compression size** (two gates: inside `ccr::attach` + final write-back at the orchestration layer). UTF-8 safe.
- **lossy semantics (spec §4.0)**: `lossy=true` ⟺ substantive content was removed. Pure format restructuring (JSON minify / csv-schema / table-to-JSON / collapsing consecutive blank lines / RLE of consecutive duplicates) is `lossy=false` and does not attach CCR; sampling / line removal / match removal / truncation / blob folding is `lossy=true` and attaches CCR.
- **Two ironclad invariants**: `kind=Json ⟹ lossy=false` (JSON/Tabular never attach CCR); `lossy=true ⟹ kind=Text` (`attach` only produces a Text bare placeholder, does not accept kind, no JSON injection).
- **CCR core rule (spec §4.7)**: under `ccr.enabled=true`, "lossy is bound to retrievable" — an `attach` result is either "successfully persisted + placeholder containing the path" or "returns the original content (abandoning compression)"; **a lossy product without a retrievable path must never occur**. Only `enabled=false` permits "lossy but not retrievable".
- **Config fail-safe**: if `~/.codex/config-zmod.toml` has no `[llm_compress]` section or `enabled=false` → fully disabled, zero-change path.

---

## Development-Time Build and Test (inherited from v1, CLAUDE.md Case B)

`zmod/llm-compress` has reverse dependencies on codex-api/codex-protocol, so it must be symlinked into the codex-rs workspace as a member in order to run `[dev-dependencies]` + `tests/*.rs`. dev-only scaffolding (not committed into the codex-rs subtree, not part of any patch):

```bash
# In the repo root, rebuild if the symlink/member is not in place (git reset --hard removes the members line; the symlink survives because it is gitignored)
ln -s ../zmod/llm-compress codex-rs/llm-compress 2>/dev/null || true
# Confirm that the [workspace] members in codex-rs/Cargo.toml contains "llm-compress" (if not, add a line "llm-compress", right after the first line of the members array)
grep -q '"llm-compress"' codex-rs/Cargo.toml || echo 'Need to manually add "llm-compress", to the members in codex-rs/Cargo.toml'
```

**Unified test command for all tasks**: `cd codex-rs && cargo test -p codez-llm-compress` (or `--test <name>` to run a single file). Use `cargo clippy -p codez-llm-compress --all-targets` for lint.

---

## Task Dependency Graph

```
01 Interface foundation (router+shared type skeleton+config+schema) ─┬─> 05 JSON upgrade
02 query + score ────────────────────────────────────────────────────┼─> 06 Search
03 command ───────────────────────────────────────────────────────────┼─> 07 Tabular
04 preprocess + protect ──────────────────────────────────────────────┼─> 08 Log rewrite
                                                                       └─> 09 Truncate/Diff wrap-up
10 ccr ────────────────────────────────────────────────────────────────────> 11 lib orchestration + fixture + parity
```

- **01** is the interface foundation: defines all cross-module shared types (`ContentKind`/`CompressOutcome`/`Budget`/`CommandHint`/`RequestCtx`/`CcrRegistry`), revises the `Compressor` trait + `compress_text` + full config expansion + `schema.rs`, and **synchronizes the signatures of the existing 4 compressors** so the crate compiles. All subsequent tasks depend on it.
- **02–04** are shared primitives/modules, depend on 01's types, and are mutually independent and parallelizable.
- **05–09** are the individual compressors, depend on 01–04, and are mutually independent and parallelizable.
- **10** ccr, depends on 01's `RequestCtx`/`CcrRegistry`/`CcrCfg`.
- **11** lib orchestration wires everything together + inherited fixtures + parity_test wrap-up; depends on all.

---

## Task List

| # | File | Deliverable | spec |
|---|------|-------------|------|
| 01 | `2026-06-21-llm-compress-v2-01-foundation.md` | router interface + shared types + config expansion + schema.rs + existing compressor signature sync | §4.0/§4.1/§4.8/§6 |
| 02 | `2026-06-21-llm-compress-v2-02-query-score.md` | `query.rs` + `score.rs` shared primitives | §4.2/§4.4 |
| 03 | `2026-06-21-llm-compress-v2-03-command.md` | `command.rs` call_id→CommandHint index | §4.3 |
| 04 | `2026-06-21-llm-compress-v2-04-preprocess-protect.md` | `preprocess.rs` (incl. blob_fold) + `protect.rs` | §4.5/§4.6 |
| 05 | `2026-06-21-llm-compress-v2-05-json.md` | JSON upgrade: detect deferral + RLE + csv-schema | §5① |
| 06 | `2026-06-21-llm-compress-v2-06-search.md` | `search.rs` SearchCompressor | §5② |
| 07 | `2026-06-21-llm-compress-v2-07-tabular.md` | `tabular.rs` TabularCompressor | §5④ |
| 08 | `2026-06-21-llm-compress-v2-08-log.md` | Log rewrite: template mining + level scoring | §5⑤ |
| 09 | `2026-06-21-llm-compress-v2-09-truncate-diff.md` | Truncate blob removal + Diff lossy-marking wrap-up | §5③/§5⑥ |
| 10 | `2026-06-21-llm-compress-v2-10-ccr.md` | `ccr.rs` persistence + placeholder + sanitize + dual limit | §4.7 |
| 11 | `2026-06-21-llm-compress-v2-11-orchestration-parity.md` | lib orchestration wiring + inherited fixtures + parity_test | §2/§8/§9 |

## Success Criteria (after the entire plan is complete, against spec §9.2)

1. All six compressors + preprocessing + protection gate + CCR are in place; the routing priority `Json→Search→Diff→Tabular→Log→Truncate` is in effect; command hints can reorder candidates.
2. The inherited-fixture `parity_test` is fully green (§8.3 four invariant classes).
3. Hard invariants hold: post-compression ≤ pre-compression, valid UTF-8, JSON products parse, only the two variants are touched, images untouched, byte-for-byte unchanged when `enabled=false`.
4. Under CCR `enabled=true`, any lossy result must be retrievable; not required when `enabled=false`; parity runs with enabled fixed on.
5. The transform signature and the integration-point patch are unchanged.
6. fail-open is fully covered.
