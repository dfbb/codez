# llm-switch: Multi-Model Routing by Purpose

Route codex's **internal sub-tasks** to different model backends — without changing the main model's behavior at all.

This is the lightweight take on "mix and match models, orchestrate them by purpose": codex keeps using your main model for the conversation, but the auxiliary LLM calls it makes on its own (compressing context, reviewing code, consolidating memory) get routed to whatever model you configured for that *purpose* — typically a cheaper or more specialized backend. No new tools are exposed to the main model, and no agent-delegation machinery is involved.

## What gets routed

codex makes several kinds of internal LLM calls beyond the main conversation. Three are routable in this version, each identified by a fixed **purpose** key:

| Purpose key | codex internal task | Why route it |
|---|---|---|
| `compact` | Context compression (auto-compact) — mechanically summarizes history when the window fills | High-volume, mechanical → a cheap fast model saves a lot of main-model tokens |
| `review`  | Code review sub-agent (`/review`, auto-review) | You may prefer a different model's review style |
| `memory`  | Memory extraction + consolidation | Background bookkeeping → fits a cheap model |

Anything else — the main conversation, and internal calls not in this list — is **never** touched by purpose routing.

## Configuration

Purpose routing lives under the existing `[llm-switch]` table in `~/.codex/config-zmod.toml`. You define backends once under `[llm-switch.providers.<id>]` (the same provider blocks llm-switch already uses), then add a `[llm-switch.purpose]` table mapping each purpose to a provider **id**.

```toml
[llm-switch]
enabled = true

# Backend definitions (reused by both provider-id routing and purpose routing).
[llm-switch.providers.deepseek-cheap]
connector = "chat"                       # "chat" | "anthropic"
base_url  = "https://api.deepseek.com/v1"
auth      = "bearer"                     # "bearer" | "x-api-key"
key_env   = "DEEPSEEK_API_KEY"           # env var holding the key (never inline the key here)
model     = "deepseek-v3"                # overrides the model name in the request

[llm-switch.providers.claude-sonnet]
connector         = "anthropic"
base_url          = "https://api.anthropic.com"
auth              = "x-api-key"
key_env           = "ANTHROPIC_API_KEY"
anthropic_version = "2023-06-01"
model             = "claude-sonnet-4-5"

# Purpose -> provider id. The value MUST be an id defined under providers above.
[llm-switch.purpose]
compact = "deepseek-cheap"
review  = "claude-sonnet"
memory  = "deepseek-cheap"
```

Rules:

- The `[llm-switch.purpose]` keys are exactly `compact`, `review`, `memory`. Other keys are ignored.
- A value must name a provider id already defined under `[llm-switch.providers.*]`. Multiple purposes may point at the same provider (above, `compact` and `memory` both use `deepseek-cheap`).
- You only configure the purposes you care about. Omit a purpose and that internal task keeps using the main model.
- Provider blocks are exactly the ones llm-switch already documents — see the main README's "Feature 1: llm-switch" for the full field reference (`connector`, `base_url`, `auth`, `key_env`, `path`, `model`, `anthropic_version`, `default_max_tokens`).

## How routing decides (two-level fallback)

For every request, the decision is **purpose → provider-id → native**, falling down the chain — never jumping straight to native from a partial match:

1. **Purpose match.** If the request is one of the routable internal tasks *and* `[llm-switch.purpose]` maps that purpose to an existing provider, the request goes to that provider's backend.
2. **Provider-id fallback.** If there's no purpose match (not a routable task, purpose unmapped, or the mapping points at an unknown provider), llm-switch falls back to its original behavior: route by the session's `model_provider_id` if that provider is configured.
3. **Native.** If neither matches, the request goes out unchanged on codex's native Responses path.

This is fail-safe by construction: a missing `[llm-switch.purpose]` table, a typo'd provider id, or `enabled = false` all simply mean "this request isn't purpose-routed" — never an error or a crash.

### Namespace tools: purpose routing yields gracefully

llm-switch's `chat`/`anthropic` connectors can't express codex's "namespace" tools (e.g. `mcp__*` MCP tools). On the main session that's a deliberate loud failure — if you point your main provider at a chat backend, you're told to disable namespace tools.

Purpose routing treats it differently. The `review` sub-agent carries your full tool set, including any MCP tools you've configured. If a purpose-routed request contains namespace tools the target backend can't express, llm-switch **abandons purpose routing for that request and falls back to provider-id routing** (logged once via `tracing::warn`) instead of hard-failing. In the common setup — main model on native OpenAI, `review` pointed at a cheap chat backend — that means the request quietly runs on the native main model: you lose the cost saving for that one review turn, but nothing breaks. The main-session (provider-id) hard-failure contract is unchanged.

### WebSocket transport

codex prefers a WebSocket transport when the provider supports it (OpenAI does by default). Purpose routing only takes effect on the HTTP path, so when a request is destined for a purpose backend, llm-switch signals codex to skip WebSocket and use HTTP for that request — otherwise purpose routing would be silently bypassed. This is automatic; there's nothing to configure. The only visible effect is that purpose-routed requests don't use the WebSocket fast path (they were going to a different backend anyway).

## A worked example

Goal: keep GPT as the main model, but compress context and consolidate memory on cheap DeepSeek, and run code review on Claude.

1. Set your backend keys in the environment:
   ```bash
   export DEEPSEEK_API_KEY=sk-...
   export ANTHROPIC_API_KEY=sk-ant-...
   ```
2. Put the config above in `~/.codex/config-zmod.toml`.
3. Use codex normally. The main GPT conversation is untouched. When codex auto-compacts, that summarization call goes to DeepSeek; when you run `/review`, it goes to Claude; memory consolidation goes to DeepSeek.

To verify routing is firing, run codex with llm-switch's `tracing` output visible and watch for the warn lines on fallback, or confirm token usage on your main model drops during compaction-heavy sessions.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| A purpose seems to still hit the main model | The purpose isn't in `[llm-switch.purpose]`, or its value names a provider id that doesn't exist (check spelling against `[llm-switch.providers.*]`). Both fall back silently. |
| Nothing is routed at all | `enabled = false`, or `~/.codex/config-zmod.toml` is missing/unparseable (fail-safe disables the whole feature). |
| `review` falls back when you expected the cheap backend | The review request carried namespace/MCP tools the target chat/anthropic backend can't express, so it fell back (see "Namespace tools" above). Expected and lossless. |

## See also

- Main README, "Feature 1: llm-switch" — connector/auth/provider field reference and the original (provider-id) routing.
- The purpose-routing design spec under `docs/superpowers/specs/` — full rationale, the source-visibility analysis behind which internal tasks are routable, and the namespace/WebSocket decisions.
