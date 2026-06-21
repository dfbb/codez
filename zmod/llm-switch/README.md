# codez-llm-switch

codez's first zmod feature crate. It **takes over the LLM API layer inside the codex process**, letting codex connect to non-OpenAI models such as DeepSeek and Claude.

- Package name: `codez-llm-switch`　lib target: `codez_llm_switch`
- Corresponding patches: build integration `patches/001-build.patch` (shared) + code hook-in `patches/002-llm-switch.patch`
- Design doc: `docs/superpowers/specs/2026-06-20-llm-switch-design.md`

## Purpose

codex **always speaks only the OpenAI Responses protocol** to `base_url` (the `WireApi` enum currently has only `Responses`). To connect upstreams like Anthropic / DeepSeek, you must translate protocols between codex and the real upstream. This crate hooks into codex client's HTTP send boundary (`stream_responses_api` in `core/src/client.rs`) and does two things:

1. **Outbound**: translate the `ResponsesApiRequest` that codex assembled (a native Responses type) into the target protocol's request body (Chat Completions or Anthropic Messages).
2. **Inbound**: read the upstream SSE, translate it event by event back into codex's `ResponseEvent`, and return a `codex_api::ResponseStream` of the **same type** as `ApiResponsesClient::stream_request`, handing it back to core's existing `map_response_stream` wrapper.

```
codex (Responses) ──run()──▶ ① transform layer (v1 pass-through) ──▶ ② Connector egress translation + HTTP/SSE ──▶ real upstream
        ▲                                                                                  │
        └────────────────  ResponseEvent ◀── SSE translation ◀── upstream SSE  ◀───────────┘
```

The routing key is codex's **`model_provider_id`** (not `name` or `base_url`): if it matches a provider configured in config-zmod, the crate takes over; otherwise it returns `None` and falls through to codex's native Responses branch (keeping the telemetry chain intact).

## Supported connectors

| connector | target protocol | default egress path | auth |
| --- | --- | --- | --- |
| `chat` | Chat Completions (DeepSeek / OpenAI compatible) | `/chat/completions` | `bearer` → `Authorization: Bearer <key>` |
| `anthropic` | Anthropic Messages | `/v1/messages` | `x-api-key` → `x-api-key: <key>` + `anthropic-version` |

`connector = "responses"` or any provider not listed in config-zmod → no takeover, falls through to the native branch.

Egress URL = `base_url.trim_end_matches('/') + path`; `base_url` is written only up to the API root (a version prefix like `/v1` counts as part of base_url), and `path` can be overridden via config.

## v1 capability boundary

v1 **supports only standard `function` tools** and plain-text conversation. In the following cases the connector always **hard-fails** with an `ApiError` (it never silently drops, never force-translates into a function):

- Non-standard tools: tool definitions or corresponding history items such as `namespace` / `custom` / freeform / `tool_search` / `image_generation`.
- Encrypted content: `EncryptedContent` / `Compaction` / `ContextCompaction`.
- A forced `tool_choice` that the target protocol cannot express equivalently.

> **Extra capabilities of the anthropic connector (only for `connector = "anthropic"`)**: The Anthropic Messages API natively supports web search and image recognition, so the anthropic connector no longer hard-fails on these two and instead translates them:
>
> - **web_search**: codex's `{"type":"web_search"}` hosted tool → Anthropic's native `{"type":"web_search_20250305","name":"web_search"}` server tool (parameters on the Responses side such as `external_web_access` / `filters` / `user_location` have no Anthropic counterpart and are dropped). This server tool is executed by the Anthropic backend and streamed back as `server_tool_use` / `web_search_tool_result` content blocks; the connector ignores them on the SSE return path (producing no extra `FunctionCall`), and the model's final text based on the search results passes through normally. **No anthropic-beta header is required.**
> - **Image recognition (vision)**: `InputImage { image_url }` (in a user message or a tool result) → Anthropic image content block. `data:<media_type>;base64,<data>` → `source.type=base64`; `http(s)://...` → `source.type=url`; any other form still hard-fails. The `detail` field is dropped (Anthropic has no equivalent field; resolution is handled automatically by the backend). Anthropic **does not support** image generation (`image_generation`), so that capability stays off for anthropic.
> - **tool_search is not yet supported**: codex's `tool_search` is a local executor and is incompatible with Anthropic's backend `tool_search_tool_*` + `defer_loading` mechanism; force-translating it would produce a non-working tool, so the anthropic connector still hard-fails on codex's `tool_search`.
>
> The filtering behavior of the chat connector and of non-taken-over providers is completely unchanged (image input / web_search etc. still hard-fail).

> **Source-level downgrade of hosted tools**: codex enables the `namespace_tools` / `web_search` / `image_generation` capabilities by default (all `true`), bundling tools for multi-agent/collaboration, web search, image generation, etc. into the Responses request (`{"type":"namespace"}` / `{"type":"web_search"}` and so on), which trips the hard-fails above. To handle this, `002-llm-switch.patch` adds a `provider_capabilities()` wrapper in `core/src/tools/spec_plan.rs` — when a taken-over provider is configured with `captype = "chat"` (the default), it is treated as having "no hosted capabilities at all" (all three capabilities `false`), reusing codex's native capability gating to avoid producing these hosted tools at the source. The connector's hard-fails remain as a backstop safety net. As a result, when actually running against a standard third-party provider you **do not** need to manually disable multi-agent / search / image features.
>
> **Exception — the anthropic connector re-enables web_search**: a provider using `connector = "anthropic"`, even with `captype = "chat"` (the default suppress), still has `provider_capabilities()` set `web_search` to `true` (while `namespace_tools` / `image_generation` stay off) — because the anthropic connector can translate web_search into Anthropic's native server tool. See `codez_llm_switch::allow_anthropic_web_search()` for the decision. The chat connector and non-taken-over providers are unaffected.
>
> If some upstream egress still speaks the Responses protocol and can handle these hosted tools itself, configure it with `captype = "response"` to pass codex's native capabilities through without any suppression.

Safely downgradable or droppable:

- `Reasoning` items and `CompactionTrigger` in history → dropped on outbound (**only the request copy is modified, codex's local history is untouched**).
- Request-level `reasoning` config → chat `reasoning_effort` / anthropic `thinking`; `text.format` → chat `response_format` / anthropic appends a system instruction.
- `store` / `include` / `prompt_cache_key` / `service_tier` / `client_metadata` → silently dropped.
- Structural breakage caused by compaction (orphan tool_call/result, tool_choice with empty tools, misplaced chat tool messages) → auto-repaired (replicating llm-rosetta), not hard-failed.

## Configuration and usage

All zmod features are controlled by `~/.codex/config-zmod.toml`, which sits alongside codex's own `~/.codex/config.toml`. Each provider needs configuration in **two places**.

### 1. codex `~/.codex/config.toml`

Configure the provider as usual; `wire_api` must be `responses` (codex only speaks Responses internally):

```toml
[model_providers.deepseek]
name     = "DeepSeek"
base_url = "https://api.deepseek.com/v1"   # not used for routing under takeover semantics
wire_api = "responses"
env_key  = "DEEPSEEK_API_KEY"

[model_providers.claude]
name     = "Claude"
base_url = "https://api.anthropic.com"
wire_api = "responses"
env_key  = "ANTHROPIC_API_KEY"
```

To switch, set `model_provider = "deepseek"` (or `"claude"`).

### 2. codez `~/.codex/config-zmod.toml`

The table name = codex's `model_provider_id`, which determines routing and egress translation:

```toml
[llm-switch]
enabled = true

[llm-switch.providers.deepseek]
connector = "chat"
base_url  = "https://api.deepseek.com/v1"   # optional; defaults to the codex provider's base_url
auth      = "bearer"
key_env   = "DEEPSEEK_API_KEY"              # connector reads the raw key itself
# path    = "/chat/completions"             # optional, overrides the default egress path
# model   = "deepseek-v4-pro"               # optional, overrides the model name sent upstream

[llm-switch.providers.claude]
connector          = "anthropic"
base_url           = "https://api.anthropic.com"
auth               = "x-api-key"
key_env            = "ANTHROPIC_API_KEY"
anthropic_version  = "2023-06-01"
default_max_tokens = 8192                    # anthropic max_tokens fallback (default constant 4096)
```

Field reference:

| field | required | description |
| --- | --- | --- |
| `connector` | yes | `chat` / `anthropic` / `responses` (responses does not enter the routing table) |
| `captype` | no | `chat` (default) suppresses hosted-tool capabilities; `response` passes codex's native capabilities through (see below) |
| `base_url` | no | egress API root; falls back to the codex provider's base_url |
| `auth` | yes | `bearer` / `x-api-key` |
| `key_env` | no* | reads the raw key from the environment variable (the runtime primary path) |
| `path` | no | overrides the default egress path |
| `model` | no | overrides the model name sent upstream; defaults to the model in the codex request |
| `anthropic_version` | no | the version header under the x-api-key form, defaults to `2023-06-01` |
| `default_max_tokens` | no | anthropic `max_tokens` fallback, defaults to 4096 |
| `context_window` | no | overrides codex's context window (tokens) for this model. Most taken-over third-party models are not in codex's built-in table and go through fallback (hard cap 272k); setting this value, via the patch, raises both `max_context_window` and the window in `with_config_overrides`, bypassing the clamp. Example: `1000000` |
| `model_catalog_json` | no | path to a model catalog JSON specific to this provider (supports `~`). When this provider is used, codex treats that table as the model catalog, so third-party models appear in the `/model` list with reasoning levels. Example: `~/.codex/model-catalog-deepseek.json` |

> **How `context_window` is implemented**: codex uses fallback metadata for unknown models (`max_context_window = 272_000`), and its top-level `model_context_window` override gets clamped to that ceiling. `002-llm-switch.patch` adds `force_context_window` to `ModelsManagerConfig`, populated by `core`'s `to_models_manager_config()` from `codez_llm_switch::context_window(provider_id)`, setting **both** `context_window` and `max_context_window` (no clamp) in `with_config_overrides`, thereby breaking past 272k.

> **How `model_catalog_json` is implemented**: the `/model` list is mapped by `build_available_models` from the model catalog; third-party slugs are not in codex's built-in table and go through fallback (`visibility=None`, empty `supported_reasoning_levels`), so they neither appear in the list nor offer a reasoning level. When `core` loads config, `002-llm-switch.patch` checks whether `codez_llm_switch::model_catalog_json(provider_id)` returns a path; if so, it reads it with `load_llm_switch_model_catalog` and overrides `config.model_catalog` — so the subsequent `StaticModelsManager` uses that table as its catalog, and the model enters `/model` with `visibility=list` and `supported_reasoning_levels`. The catalog JSON is codex's `ModelsResponse` (`{"models":[{slug,display_name,visibility:"list",supported_reasoning_levels,context_window,...}]}`; for required fields see the `~/.codex/model-catalog-*.json` example).

### fail-safe and toggles

- File missing, `[llm-switch]` section missing, `enabled = false`, or provider not matched → entirely **off**, falls through to the native Responses branch.
- Config parse error → log a `warn` and turn off, without making codex fail to start.

### Key sources (priority order)

1. `key_env` → `std::env::var(key_env)` reads the raw key (**the runtime primary path**).
2. `auth_key` inline plaintext → **only allowed in the gitignored `tests/testkey.toml`**; once `auth_key` appears in a real `config-zmod.toml`, parsing errors out and refuses to start.
3. Fallback only when `auth = "bearer"` and no key is configured: reuse codex's `api_auth.add_auth_headers()` to write `Authorization: Bearer`. The `x-api-key` form has **no** such fallback and must have `key_env` / `auth_key`.

## Integration with codex-rs (patch)

This crate reverse-depends on `codex-api` / `codex-protocol` (path dependencies, see `Cargo.toml`), making it "case B" as described in CLAUDE.md. Production integration is split across two patches: build integration in the shared `patches/001-build.patch`, code hook-in points in `patches/002-llm-switch.patch`. Touchpoints:

1. **`001-build.patch`** → `core/Cargo.toml`: adds `codez-llm-switch = { path = "../../zmod/llm-switch" }` (a plain path dependency, not entered into workspace members).
2. **`002-llm-switch.patch`** → `core/src/client.rs`: `ModelClient::new` gains a parameter `model_provider_id: String`, stored into `ModelClientState` (all call sites, including `memories/write`, are updated accordingly).
3. **`002-llm-switch.patch`** → `stream_responses_api` in `core/src/client.rs`: based on `codez_llm_switch::route(...)`, choose one of two — `None` goes to the native `ApiResponsesClient` (keeping `.with_telemetry(...)`), `Some(rt)` goes to `codez_llm_switch::run(...)`, both landing in the same `match stream_result`.

> Known gap: the takeover path does not connect to codex-api's request/SSE telemetry (the connector uses its own HTTP/SSE client); both the LastResponse/cancellation paths of `inference_trace` and `map_response_stream` are preserved.

## Public API

```rust
// Routing decision (the patch calls this inside stream_responses_api)
pub fn route(model_provider_id: &str) -> Option<Route>;
pub fn enabled() -> bool;

// Takeover entry point (the call contract for the patch; signature is fixed)
pub async fn run(
    rt: Route,
    request: codex_api::ResponsesApiRequest,
    api_auth: codex_api::SharedAuthProvider,
) -> Result<codex_api::ResponseStream, codex_api::ApiError>;

// Configuration
pub fn load_config_from_str(toml_text: &str, allow_inline_key: bool) -> Result<Config, ConfigError>;
pub fn load_testkey_config(path: &Path) -> Result<Config, ConfigError>;  // tests only, allows inline auth_key
```

At runtime it reads `~/.codex/config-zmod.toml` (or `$CODEX_HOME/config-zmod.toml`) once and caches it process-wide.

## Module layout

```
src/
  lib.rs            run()/route()/enabled() entry points; config cache
  config.rs         parses the [llm-switch] section of config-zmod
  http.rs           egress URL assembly + auth header shaping + key resolution
  pipeline.rs       TransformPlugin trait + ordered execution (v1 pass-through)
  transform/        landing spot for future compression transforms (empty in v1)
  sse.rs            egress HTTP request + SSE byte reading (safe across multi-byte chunks)
  connector/
    mod.rs          Connector trait + EgressCtx + factory
    chat.rs         chat connector; chat_req.rs request construction; chat_sse.rs SSE translation
    anthropic.rs    anthropic connector; anthropic_req.rs request construction; anthropic_sse.rs SSE translation
```

## Build and test

This crate lives **outside** the codex-rs workspace and reverse-depends on its crates, subject to two cargo hard constraints: as a "non-member path dependency" it cannot use `[dev-dependencies]` and cannot run `tests/*.rs`; yet cargo also rejects a member outside codex-rs.

**Development-time workaround** (CLAUDE.md "case B development testing"): use a symlink to temporarily wire this crate into the codex-rs workspace as a real member, thereby enabling dev-deps (wiremock) and integration tests, sharing codex-rs's `Cargo.lock` / `target`.

```bash
# at the repo root
ln -s ../zmod/llm-switch codex-rs/llm-switch          # symlink (already covered by .gitignore)
# add a line at the end of [workspace] members in codex-rs/Cargo.toml: "llm-switch",

cd codex-rs
cargo test -p codez-llm-switch                         # all tests
cargo test -p codez-llm-switch --test chat_request_test # a single integration test
cargo clippy -p codez-llm-switch --all-targets         # lint
```

> Discipline: the symlink `codex-rs/llm-switch`, the members line in `codex-rs/Cargo.toml`, and the build-generated changes to `codex-rs/Cargo.lock` are all **dev-only scaffolding**; keep them uncommitted dirty and **never** commit them into the codex-rs subtree, and **never** put them into any patch. `git reset --hard` will revert the members line (the symlink survives because it is ignored); rebuild it with the two steps above.

### Live tests (gated)

`tests/live_test.rs` hits the real DeepSeek / Claude endpoints and is `#[ignore]` by default. After configuring the provider + `auth_key` + `model` in `tests/testkey.toml` (**gitignored, contains real keys, must not be committed**):

```bash
cargo test -p codez-llm-switch -- --ignored
```

When `testkey.toml` is missing the tests are skipped automatically, so CI stays green even without keys. The offline golden tests (request construction / SSE translation / config / hard-fail / downgrade assertions) need no key.
