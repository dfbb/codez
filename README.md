# codez

codez is a derivative project built on the `codex-rs` directory of the [openai/codex](https://github.com/openai/codex) repository. It syncs the upstream Rust workspace in verbatim, then layers codez's own features, scripts, and docs on top in a **non-invasive** way.

Two features have landed so far:

- **llm-switch** — lets codex talk to non-OpenAI upstreams such as Anthropic and DeepSeek, and routes internal sub-tasks (compact / review / memory) to different models by purpose.
- **llm-compress** — compresses tool-call outputs in place at the LLM request boundary to cut token consumption.

## Design: patch + sync, features isolated in zmod

codez is not a GitHub fork. It uses `git subtree --squash` to bring upstream `codex-rs` in as a **read-only snapshot**. The core constraint: **never modify `codex-rs/` source directly** — otherwise every upstream sync would produce three-way merge conflicts.

To that end, codez isolates all of its own changes into two directories:

| Directory | Responsibility | Maintained by |
| --- | --- | --- |
| `codex-rs/` | Snapshot of upstream `openai/codex`'s `codex-rs` (~494 crates) | Pulled by sync scripts, **not hand-edited** |
| `zmod/` | codez's new features, one standalone Rust crate per feature | codez |
| `patches/` | Minimal invasive patches applied to `codex-rs`, one per zmod crate | codez |
| `scripts/` | Sync and Git workflow scripts | codez |
| `docs/` | codez's own design docs | codez |

**Build model**: first apply all `patches/*.patch` to `codex-rs` (the patches wire each zmod crate into the build and insert call sites into upstream code), then `cargo build` `codex-rs` + `zmod` together.

**Naming convention**: each feature binds one crate and one patch under the same kebab-case short name `<feature>` — directory `zmod/<feature>/`, package name `codez-<feature>`, patch `patches/<feature>.patch`. One feature maps to exactly one patch.

**Handling reverse dependencies**: both llm-switch and llm-compress reverse-depend on codex-rs crates (`codex-api`, `codex-protocol`). Such crates are **not added** to the workspace `members` (crossing workspace roots is error-prone); instead they point back via explicit path dependencies, and a patch adds an external path dependency in the codex-rs crate that needs it (e.g. `core/Cargo.toml`) so it gets compiled along with it. See [CLAUDE.md](CLAUDE.md) for details.

**Single integration point**: both features hook into the send boundary of `stream_responses_api()` in `codex-rs/core/src/client.rs`, with orthogonal responsibilities — llm-compress intercepts first (compress), llm-switch routes afterward (forward or pass through). The patch's intrusion into codex is limited to Cargo dependencies, a few call sites, and one routing-key parameter; it does not change any downstream error handling, telemetry, or stream-mapping logic.

## Runtime configuration: `~/.codex/config-zmod.toml`

All zmod features are controlled by `~/.codex/config-zmod.toml` and can be **toggled independently** without affecting each other. This file sits alongside codex's own `~/.codex/config.toml` but carries only zmod configuration.

Each feature has a table named after its `<feature>`, with at least an `enabled` switch. **When the file or a table is missing, the corresponding feature defaults to off (fail-safe)**; feature code does not error when it can't read config — it treats that as disabled.

```toml
# ~/.codex/config-zmod.toml
[llm-switch]
enabled = true

[llm_compress]
enabled = true
```

## Feature 1: llm-switch

Lets codex talk to non-OpenAI upstreams. Internally codex only speaks the Responses protocol; llm-switch performs **in-process protocol translation** before the request leaves for upstream, without spinning up a separate proxy process.

**Protocol coverage (v1)**:

- **chat** — Responses ⇄ Chat Completions (DeepSeek / OpenAI-compatible), Bearer auth.
- **anthropic** — Responses ⇄ Anthropic Messages, `x-api-key` + `anthropic-version` auth.
- **responses** — pass-through, skips zmod routing, fully preserves codex-api's native telemetry.

**Purpose routing**: inspired by "mix and match models by purpose." codez takes the lightweight form — the main model's behavior is completely unchanged and no new tools are added; it only routes codex's **internal sub-tasks** (compact context compression, review code review, memory consolidation) each to the model configured for that purpose.

**Two-level fallback (fail-safe)**: `purpose mapping → route by provider_id → native main model`. A miss at any level falls down the chain rather than jumping straight to native. When purpose routing matches but the request contains namespace tools that llm-switch cannot express, it **abandons purpose routing and falls back to provider-id routing** (not a hard failure); provider-id routing (the main session) keeps v1's "loud hard failure" contract unchanged.

```toml
[llm-switch]
enabled = true

[llm-switch.providers.deepseek-cheap]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"
model     = "deepseek-v3"

[llm-switch.providers.claude-sonnet]
connector = "anthropic"
base_url  = "https://api.anthropic.com"
auth      = "x-api-key"
key_env   = "ANTHROPIC_API_KEY"
model     = "claude-sonnet-4-5"

# purpose -> provider id mapping (value must be an id already defined in providers above)
[llm-switch.purpose]
compact = "deepseek-cheap"
review  = "claude-sonnet"
memory  = "deepseek-cheap"
```

For design details see [docs/superpowers/specs/2026-06-20-llm-switch-design.md](docs/superpowers/specs/2026-06-20-llm-switch-design.md) and [the purpose-routing design](docs/superpowers/specs/2026-06-22-llm-switch-purpose-routing-design.md).

## Feature 2: llm-compress

Before codex's assembled request leaves for upstream, conservatively compresses tool-call outputs to shrink token volume. Compression applies to **all** request paths (including the native OpenAI responses path) and does not depend on whether llm-switch matches.

**Content routing + six compressors**, claimed in a fixed priority order; the first to match owns the compression:

| Priority | Compressor | Claim condition | Method |
| --- | --- | --- | --- |
| 1 | Json | Valid JSON object/array | Consecutive RLE dedup + csv-schema restructure (lossless) |
| 2 | Search | grep/rg-style results | Group by file, keep head/tail + scored-selected segments (lossy) |
| 3 | Diff | git diff / unified diff | Collapse large context blocks (lossy) |
| 4 | Tabular | CSV/TSV/Markdown tables | csv-schema restructure (lossless) |
| 5 | Log | Log lines | Level-scored retention (keep error/warn/stack frames; lossy) |
| 6 | Truncate | Arbitrary text (fallback) | Keep head/tail + truncate middle (lossy) |

**Safety guarantees**: fail-open throughout — a compressor panic is caught by `catch_unwind` and that item passes through verbatim; a size gate guarantees the written-back result is ≤ original; compressed JSON must re-parse, otherwise it falls back to the original. Lossy compression writes the original into the CCR archive (`~/.codex/llm-compress/ccr/`) and inserts a retrieval hint at the head of the result for on-demand retrieval.

```toml
[llm_compress]
enabled = true
min_total_bytes = 4096     # if total compressible text in a request is below this -> skip entirely
per_item_min_bytes = 512   # skip items below this byte count
```

For the full config options and compressor strategies see [zmod/llm-compress/README.md](zmod/llm-compress/README.md) and the design docs [v1](docs/superpowers/specs/2026-06-20-llm-compress-design.md) / [v2](docs/superpowers/specs/2026-06-21-llm-compress-v2-design.md).

## Build and test

`codex-rs` is a standard Cargo workspace, pinned to the Rust `1.95.0` toolchain. Run commands inside the `codex-rs/` directory:

```bash
cd codex-rs
cargo build                          # build the whole workspace
cargo build -p codez-llm-switch      # build a single zmod crate
cargo nextest run                    # run all tests (the repo uses nextest)
cargo clippy --all-targets           # lint
cargo fmt                            # format
```

zmod crates that reverse-depend on codex-api (llm-switch / llm-compress) are, during development, symlinked into the codex-rs workspace as members to run their own tests. See the "Case B development-time testing" section in [CLAUDE.md](CLAUDE.md).

## Sync and Git workflow

Remote conventions (see [scripts/git/README.md](scripts/git/README.md) for details):

- `origin` = `git@github.com:dfbb/codez.git`, codez's **only** push target.
- `upstream-codex` = `https://github.com/openai/codex.git`, **read-only**, push URL pinned to `DISABLED`.

Common scripts:

```bash
scripts/git/04-sync-codex-rs.zsh main main          # sync upstream codex-rs
scripts/git/06-push-origin-slow-network.zsh main    # push to origin with progress on slow networks
```

Required after syncing: re-verify that `patches/*.patch` still apply cleanly to the new `codex-rs`; if upstream changed the code a patch targets, update the corresponding `<feature>.patch`.

## Acknowledgements

codez stands on the shoulders of [**openai/codex**](https://github.com/openai/codex) — the `codex-rs/` directory comes entirely from that project, and its Rust workspace is the runtime foundation for all of codez's features. Thanks to OpenAI and all codex contributors for open-sourcing this excellent agentic coding infrastructure.

llm-switch's protocol-translation rules reference the converter implementations of llm-rosetta / rust-llm-proxy; llm-compress's content routing and segment-filtering pipeline draw on the designs of headroom and rtk. The provenance of test fixtures inherited from third parties is listed in `zmod/llm-compress/tests/fixtures/inherited/NOTICE.md`.

codez is maintained only on `origin` (`dfbb/codez`), does not submit PRs to `openai/codex`, and does not expand upstream's full history.
