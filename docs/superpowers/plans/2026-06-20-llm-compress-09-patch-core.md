# Task 09: patch core + wiring + live verification

> Part of `2026-06-20-llm-compress-00-index.md`. Read the index Global Constraints first.
> **Key discipline: never leave changes to `codex-rs/` in the source tree as a final deliverable — all codex-rs changes must land in `patches/llm-compress.patch`, and the `codex-rs/` working tree must be restored clean** (otherwise the sync script `04-sync-codex-rs.zsh` will conflict). Depends on Task 08.

**Goal:** Generate `patches/llm-compress.patch` that wires `codez-llm-compress` into codex-rs (the **production** case-B route): ① add an external path dependency in `core/Cargo.toml`; ② insert the two lines (queryid retrieval + `transform` call) in `stream_responses_api`, after `prepare_response_items_for_request` and before `record_started`. Verify: after applying the patch `cargo build` passes, and `transform` actually compresses under a real config.

> **Distinction from the dev-time symlink member**: during development (Tasks 01–08) we use the symlink `codex-rs/llm-compress` plus the members line in `codex-rs/Cargo.toml` (dev-only scaffolding, dirty/gitignore, **not in this patch**). The production patch in this task is a **separate** wiring path — an external path dependency in `core/Cargo.toml`, unrelated to the symlink. When exporting the patch, only diff the two files `core/Cargo.toml` and `core/src/client.rs`; **never** include `codex-rs/Cargo.toml` (the members line).

**Spec coverage:** §2 (integration points), §11 (workspace wiring / patch conventions).

**Files:**
- Create: `patches/llm-compress.patch`
- Temporarily modify (only to generate the patch; restored after export): `codex-rs/core/Cargo.toml`, `codex-rs/core/src/client.rs`

**Pre-flight check (glance at the real lines first to avoid line-number drift):**
```bash
cd /Users/dfbb/Sites/skycode/codez/codex-rs
sed -n '1308,1332p' core/src/client.rs    # integration-point context: let mut request ... stream_request
grep -n "codez-llm-switch\|codez-llm-compress" core/Cargo.toml   # current state of the dependency section
```

---

- [ ] **Step 1: Add the external path dependency in core/Cargo.toml (production wiring, added for the first time in this task)**

In the `[dependencies]` of `codex-rs/core/Cargo.toml`, add (immediately after the existing `codez-llm-switch` line, if present):

```toml
codez-llm-compress = { path = "../../zmod/llm-compress" }
```

> Note: the dev-time setup is a symlink member (the members line in `codex-rs/Cargo.toml`), which is a different thing from the path dependency here in `core/Cargo.toml`. This line is the **production** wiring and goes into the patch; the members line is dev scaffolding and does not.

- [ ] **Step 2: Insert the transform call in client.rs**

In `stream_responses_api` of `codex-rs/core/src/client.rs`, locate:

```rust
            let store = request.store;
            self.client
                .prepare_response_items_for_request(&mut request.input, store);
            let inference_trace_attempt = inference_trace.start_attempt();
```

Change it to (inserting two lines after `prepare_response_items_for_request` and before `let inference_trace_attempt`):

```rust
            let store = request.store;
            self.client
                .prepare_response_items_for_request(&mut request.input, store);

            // ── llm-compress pre-interception (independent zmod; compress first, then route) ──
            let llm_compress_qid = responses_metadata.thread_id.clone();
            codez_llm_compress::transform(
                &mut request,
                &client_setup.api_provider,
                &llm_compress_qid,
            );

            let inference_trace_attempt = inference_trace.start_attempt();
```

> **Borrow note (verified in the index)**: the short borrow `&client_setup.api_provider` is dropped at the end of the transform call statement, so the later line (~1324) `ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)` can still move it by value. `responses_metadata.thread_id` is a `String` field on `&CodexResponsesMetadata` — here we `.clone()` it into `llm_compress_qid: String` and then borrow `&...`, avoiding borrow-lifetime entanglement with other uses of `responses_metadata` in the same scope (cloning a short UUID has negligible cost). The `&mut request` borrow ends at the end of the call and does not affect the subsequent `record_started(&request)` and `stream_request(request, ...)`.

- [ ] **Step 3: Compile verification (dirty working tree)**

Run (`codex-rs/`):
```bash
cargo build -p codex-core
```
Expected: compiles cleanly. If it reports `codez_llm_compress` not found → confirm the Step 1 dependency line; if it reports a borrow/move error → fix per the Step 2 note (usually the `thread_id` borrow; use the `.clone()` form from Step 2).

- [ ] **Step 4: Live verification — enable the config and run real compression**

Write a temporary config and use the crate's integration test to verify that `transform` actually compresses when enabled (without depending on a real codex run):

Run (`codex-rs/`):
```bash
HOME_BAK="$HOME"
TMPHOME="$(mktemp -d)"
mkdir -p "$TMPHOME/.codex"
cat > "$TMPHOME/.codex/config-zmod.toml" <<'EOF'
[llm_compress]
enabled = true
min_total_bytes = 64
per_item_min_bytes = 32

[llm_compress.truncate]
head_lines = 2
tail_lines = 2
max_bytes = 4096
EOF
HOME="$TMPHOME" cargo test -p codez-llm-compress --test transform_test -- --nocapture
ls -la "$TMPHOME/.codex/log/" 2>/dev/null || echo "(it is normal for this test not to trigger a log write; logs are triggered by real large inputs)"
rm -rf "$TMPHOME"
export HOME="$HOME_BAK"
```
Expected: transform_test still all green (the test itself is built mainly around the `disabled` invariant; this step mainly verifies that under the enabled config there is no panic and the crate loads the real config path correctly).

> The live end-to-end check (real codex sends a request → see lines appear in `~/.codex/log/llm-compress.log`) is left as a manual smoke test and not part of automation (to avoid depending on a real API key).

- [ ] **Step 5: Export the patch**

Run (`codex-rs/`):
```bash
git diff -- core/Cargo.toml core/src/client.rs > ../patches/llm-compress.patch
```
Check that the patch contents include only these two files, with changes being exactly the dependency line above plus the two call lines:
```bash
cat ../patches/llm-compress.patch
```

- [ ] **Step 6: Restore the codex-rs working tree (the deliverable is the patch, not modified source)**

Run (`codex-rs/`):
```bash
git checkout -- core/Cargo.toml core/src/client.rs
git status --short core/    # should be empty — codex-rs working tree is clean
```

> This step restores the production changes introduced by this task in `core/Cargo.toml` and `core/src/client.rs` (already captured in the patch). **Note**: the dev-time symlink member scaffolding (the symlink `codex-rs/llm-compress`, the members line in `codex-rs/Cargo.toml`, `codex-rs/Cargo.lock`) is **not** in scope of this checkout; it stays dirty/untracked — unless you intend to fully tear down the dev environment, leave it alone.

- [ ] **Step 7: Verify the patch applies standalone**

Run (`codex-rs/`):
```bash
git apply --check ../patches/llm-compress.patch && echo "PATCH OK"
```
Expected: `PATCH OK` (no output means a conflict; go back to Step 5 and re-export).

- [ ] **Step 8: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add patches/llm-compress.patch docs/superpowers/plans/2026-06-20-llm-compress-09-patch-core.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): patch wiring codez-llm-compress into codex-rs (transform at request boundary)"
```

- [ ] **Step 9: Wrap-up self-check**

- [ ] `patches/llm-compress.patch` exists and `git apply --check` passes.
- [ ] The `codex-rs/` working tree is clean (`git status --short codex-rs/core/` is empty).
- [ ] `cargo test -p codez-llm-compress` (either with the patch applied or in pure-crate state) is all green.
- [ ] The crate is not in the codex-rs workspace `members`; `zmod/llm-compress/Cargo.lock` is not committed.
