# Task 09 — patch wiring into codex-rs core

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or executing-plans. First read the [master index](2026-06-20-llm-switch-00-index.md) Global Constraints. **Key discipline: never modify `codex-rs/` source as the final deliverable — every change to codex-rs must land in `patches/llm-switch.patch`** (otherwise the next `04-sync-codex-rs.zsh` will inevitably conflict).

**Goal:** Generate `patches/llm-switch.patch` to wire `codez-llm-switch` into codex-rs: (1) add a path dependency in `core/Cargo.toml`; (2) add a `model_provider_id: String` parameter to `ModelClient::new` plus a field on `ModelClientState`, passed in at the call site from `Config.model_provider_id`; (3) change `stream_responses_api` so that "constructing `ApiResponsesClient` + `stream_request`" becomes a route-based choice via `route()`, producing the same `Result<codex_api::ResponseStream, ApiError>` that feeds the existing `match stream_result`. Verify: after applying the patch, `cargo build` + existing tests pass.

**Covers spec:** §6 (full patch), §2.1/§2.3/§6.2/§6.3.

**Files:**
- Create: `patches/llm-switch.patch`
- Temporary edits (only to generate the patch; restore the worktree after exporting): `codex-rs/core/Cargo.toml`, `codex-rs/core/src/client.rs`

**Interfaces:**
- Consumes (Task 08): `codez_llm_switch::route(&str) -> Option<Route>`, `codez_llm_switch::run(rt, request, api_auth) -> Result<codex_api::ResponseStream, ApiError>`, `codez_llm_switch::Route`.

---

- [ ] **Step 0: Read the real wiring points (verbatim, do this first)**

```bash
sed -n '160,230p' codex-rs/core/src/client.rs       # ModelClientState + ModelClient structs
sed -n '370,412p' codex-rs/core/src/client.rs        # ModelClient::new signature
sed -n '1270,1360p' codex-rs/core/src/client.rs      # stream_responses_api: construct client + stream_request + match stream_result
grep -rn "ModelClient::new(" codex-rs/core/src        # the single call site
grep -n "model_provider_id" codex-rs/core/src/config/mod.rs   # Config field (around line 632)
```
Record: (a) the full argument order of `ModelClient::new`; (b) how `stream_responses_api` obtains `transport` / `client_setup.api_provider` / `client_setup.api_auth` / `request` / `options` / `request_telemetry` / `sse_telemetry`; (c) each arm of `match stream_result` verbatim; (d) whether the `ModelClient::new` call site can reach `config.model_provider_id`. **Subsequent steps' code must follow the real text read here**; the snippets below are templates.

- [ ] **Step 1: Add the path dependency to `core/Cargo.toml`**

Add to `[dependencies]` in `codex-rs/core/Cargo.toml` (case B, not joining the workspace members):

```toml
codez-llm-switch = { path = "../../zmod/llm-switch" }
```

- [ ] **Step 2: Add a field to `ModelClientState` + a parameter to `ModelClient::new`**

Add to the `ModelClientState` struct:
```rust
    model_provider_id: String,
```
Add `model_provider_id: String` to the end of the `ModelClient::new` parameter list (or a semantically appropriate spot), and fill it in where `ModelClientState { ... }` is constructed. Its single call site passes `config.model_provider_id.clone()`.

> If the `ModelClient::new` call site does not have `config` on hand, fetch `model_provider_id` from the existing `provider_info` / upstream `Config` chain instead — follow the real context read in Step 0. **Do not** fall back to `provider_info.name` (§2.1).

- [ ] **Step 3: Change `stream_responses_api` into a routed choice**

Take the snippet read in Step 0 (template):
```rust
let client = ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
    .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
let stream_result = client.stream_request(request, options).await;
```
Change it to:
```rust
let stream_result = match codez_llm_switch::route(&self.state.model_provider_id) {
    None => {
        // Native path: telemetry chain fully preserved
        let client = ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
        client.stream_request(request, options).await
    }
    Some(rt) => {
        // Takeover path: the connector brings its own HTTP/SSE; api_auth is only a bearer fallback.
        // transport / api_provider / request_telemetry / sse_telemetry are unused in this arm and drop automatically when the scope ends.
        codez_llm_switch::run(rt, request, client_setup.api_auth).await
    }
};
```

> Note:
> - The real access path for `self.state.model_provider_id` follows Step 0 (it may be `self.state.model_provider_id`, with the field inside `Arc<ModelClientState>`).
> - Both arms produce `Result<codez_api::ResponseStream, ApiError>`; **the downstream `match stream_result { Ok(stream) => map_response_stream(...), Err(401) => handle_unauthorized..., Err(err) => map_api_error... }` stays untouched**.
> - `request` / `options` / `api_auth` are owned: the takeover arm moves `request` + `api_auth`, the native arm moves `request` + `options` + `api_provider` + `api_auth` + `transport`. They are mutually exclusive, so the moves are legal. If the compiler complains that `api_auth` is moved in both arms while it is not owned per-arm — use `client_setup.api_auth.clone()` (`SharedAuthProvider = Arc`, clone is cheap) in the takeover arm, leaving the native arm's move intact; follow the compiler's result.

- [ ] **Step 4: Compile-verify before producing the patch (in the temporary-edit state)**

```bash
cd codex-rs && cargo build -p codex-core
```
Expected: compilation succeeds. If the `codez-llm-switch` path dependency recompiles codex-api/protocol and causes a version conflict, double-check that Task 01's path points exactly at `codex-rs/codex-api` and `codex-rs/protocol` (the same source, so Cargo recognizes them as the same crate).

- [ ] **Step 5: Export the patch and restore the worktree**

```bash
cd codex-rs
git diff -- core/Cargo.toml core/src/client.rs > ../patches/llm-switch.patch
git checkout -- core/Cargo.toml core/src/client.rs    # restore codex-rs source; changes live only in the patch
```

> Key: the `codex-rs/` worktree must be restored clean — the deliverable is `patches/llm-switch.patch`, not modified codex-rs source. `04-sync-codex-rs.zsh` requires a clean codex-rs worktree before it runs.

- [ ] **Step 6: Verify the patch applies cleanly**

```bash
cd codex-rs && git apply --check ../patches/llm-switch.patch && echo "PATCH OK"
```
Expected: output `PATCH OK` (`--check` does not actually modify anything).

- [ ] **Step 7: End-to-end compile verification (apply patch, then build the full workspace + existing tests)**

```bash
cd codex-rs
git apply ../patches/llm-switch.patch
cargo build                              # full workspace + zmod compiled together
cargo nextest run -p codex-core          # existing core tests do not regress
git checkout -- core/Cargo.toml core/src/client.rs   # restore after verification
```
Expected: build succeeds, core tests pass. On any failure, fix `zmod/llm-switch` or the patch and re-export.

- [ ] **Step 8: Commit**

```bash
git add patches/llm-switch.patch
git commit -m "feat(llm-switch): patch wiring codez-llm-switch into codex-rs core (route + ModelClient id + stream_responses_api)"
```

> The case A/B split convention in `CLAUDE.md` was landed in `7a12f5291` (spec §6.4); this task does not need to touch CLAUDE.md again. If Step 0 reveals the convention diverges from reality, update CLAUDE.md as part of this task and register it.
