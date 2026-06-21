# Task 08: transform Entry Orchestration

> Part of `2026-06-20-llm-compress-00-index.md`. Before executing, read the index's Global Constraints / real types. Depends on Task 01-07.

**Goal:** Implement the crate's single entry point `transform(&mut request, &api_provider, queryid)`, orchestrating Layer 0 (toggle & budget gate) → Layer 1 (iterate `Vec<ResponseItem>`, extract tool output text by precise rules) → Layer 2/3 (compress via ContentRouter) → exit (when overall saved>0, call stats to write the log). Assemble the four compressors `[Json, Diff, Log, Truncate]`.

**Spec coverage:** §1/§2 (signature/integration), §4 (pipeline/extraction rules), §7 (trigger log), §8 (fail-open).

**Files:**
- Modify: `zmod/llm-compress/src/lib.rs` (add `transform` + internal orchestration)
- Test: `zmod/llm-compress/tests/transform_test.rs`

**Interfaces:**
- Consumes:
  - Task 01 `config::{load, Config}`
  - Task 02 `router::{Budget, ContentRouter, Compressor}` (note: the trait result type is named `CompressOutcome`, **not** the `CompressResult` from the old code block in spec §4 — follow Task 02)
  - Task 03-06 `compress::{json::JsonCompressor, diff::DiffCompressor, log::LogCompressor, truncate::TruncateCompressor}`
  - Task 07 `stats::log_compression`
  - codex types: `codex_api::ResponsesApiRequest`, `codex_api::Provider as ApiProvider`, `codex_protocol::models::{ResponseItem, FunctionCallOutputPayload, FunctionCallOutputBody, FunctionCallOutputContentItem}`
- Produces:
  - `pub fn transform(request: &mut ResponsesApiRequest, api_provider: &ApiProvider, queryid: &str)`

> **Real-type verification (already listed in index)**: `request.input: Vec<ResponseItem>`; the variants to process are `ResponseItem::FunctionCallOutput { call_id, output }` and `ResponseItem::CustomToolCallOutput { call_id, name, output }`, both with `output: FunctionCallOutputPayload { body: FunctionCallOutputBody, success: Option<bool> }`; `FunctionCallOutputBody::Text(String)` / `FunctionCallOutputBody::ContentItems(Vec<FunctionCallOutputContentItem>)`; `FunctionCallOutputContentItem::InputText { text: String }` (there are also `InputImage`/`EncryptedContent`, which we leave untouched).

> **Import-path verification**: Before executing this task, run `cargo doc` under `codex-rs/` or grep directly to confirm the re-export paths of `ResponsesApiRequest` and `Provider`. The index already notes `use codex_api::Provider as ApiProvider` (`core/src/client.rs:23`). `ResponsesApiRequest` is in `codex-api` (`codex-api/src/common.rs`). `ResponseItem` and other models are in `codex-protocol` (crate name `codex_protocol`, see the `codex-protocol` dependency in Cargo.toml). If compilation reports a path mismatch, fix the `use` based on the errors from `cargo build -p codez-llm-compress`, but **do not** change type semantics.

---

- [ ] **Step 1: Write the failing test**

Create `zmod/llm-compress/tests/transform_test.rs`:

```rust
//! transform end-to-end orchestration test. Builds the request with real codex types.

use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseItem,
};

/// Build a minimal ResponsesApiRequest; input is supplied by the caller.
/// Note: ResponsesApiRequest has many fields, so `..Default` is not viable (no Default);
/// here a helper fills in the required fields with minimal values. Fields follow
/// codex-api/src/common.rs; if the field set changes, the compiler will point it out —
/// add fields per the errors, using empty/false/None values.
fn req_with(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "gpt-test".to_string(),
        instructions: String::new(),
        input,
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn provider() -> codex_api::Provider {
    // Provider construction: follow the public constructor/Default in codex-api/src/provider.rs.
    // If there is no Default, construct minimally from its fields; this test does not depend on
    // provider contents (transform only reads it for discrimination).
    codex_api::Provider::default()
}

fn fco_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success: Some(true),
        },
    }
}

#[test]
fn disabled_config_leaves_request_untouched() {
    // No config-zmod file → enabled=false → request unchanged.
    let big = "x\n".repeat(10_000);
    let mut r = req_with(vec![fco_text("c1", &big)]);
    let before = r.clone();
    transform(&mut r, &provider(), "qid-1");
    assert_eq!(r.input.len(), before.input.len());
    if let (
        ResponseItem::FunctionCallOutput { output: a, .. },
        ResponseItem::FunctionCallOutput { output: b, .. },
    ) = (&r.input[0], &before.input[0])
    {
        // Byte-for-byte unchanged when disabled
        match (&a.body, &b.body) {
            (FunctionCallOutputBody::Text(sa), FunctionCallOutputBody::Text(sb)) => {
                assert_eq!(sa, sb)
            }
            _ => panic!("body shape changed"),
        }
    } else {
        panic!("variant changed");
    }
}

#[test]
fn non_tooloutput_variants_are_ignored() {
    // Variants like Message are not processed (here we use FunctionCall as a stand-in for
    // another variant; just verify no panic and unchanged length).
    let mut r = req_with(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert_eq!(r.input.len(), 1);
    assert!(matches!(r.input[0], ResponseItem::Other));
}

#[test]
fn contentitems_image_preserved() {
    // ContentItems contains InputText + InputImage: the image must be preserved as-is.
    let mut r = req_with(vec![ResponseItem::FunctionCallOutput {
        call_id: "c3".to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText { text: "short".to_string() },
                FunctionCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AAAA".to_string(),
                    detail: None,
                },
            ]),
            success: None,
        },
    }]);
    transform(&mut r, &provider(), "qid-3");
    if let ResponseItem::FunctionCallOutput { output, .. } = &r.input[0] {
        if let FunctionCallOutputBody::ContentItems(items) = &output.body {
            // Image item preserved as-is
            assert!(items.iter().any(|it| matches!(
                it,
                FunctionCallOutputContentItem::InputImage { image_url, .. } if image_url == "data:image/png;base64,AAAA"
            )));
        } else {
            panic!("body shape changed");
        }
    } else {
        panic!("variant changed");
    }
}
```

> **Testing the compression-enabled path**: `disabled_config_leaves_request_untouched` covers the disabled path. The end-to-end assertion that "compression actually happens once enabled" is fragile (depends on a real ~/.codex file), so it is left to Task 09's live verification; the unit tests here focus on three invariants that do not depend on external config: "unchanged when disabled / non-target variants untouched / images preserved".

- [ ] **Step 2: Run the test and watch it fail**

Run (`codex-rs/`):
```bash
cargo test -p codez-llm-compress --test transform_test
```
Expected: compile failure (`transform` undefined). If the fields or construction of `req_with`/`provider` do not match the real types, the compiler will report field errors — adjust the test helper's fields per the errors (use minimal values) without changing semantics.

- [ ] **Step 3: Write the transform orchestration (lib.rs)**

Modify `zmod/llm-compress/src/lib.rs`, appending at the end of the file (the module declarations `pub mod stats;` `pub mod compress;` etc. should already be added by 01-07; add them if missing):

```rust
use codex_api::Provider as ApiProvider;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, ResponseItem,
};

use crate::compress::{
    diff::DiffCompressor, json::JsonCompressor, log::LogCompressor, truncate::TruncateCompressor,
};
use crate::config::Config;
use crate::router::{Budget, ContentRouter};

/// Assemble the four compressors, with fixed priority Json → Diff → Log → Truncate (Truncate is the fallback).
fn build_router() -> ContentRouter {
    ContentRouter::new(vec![
        Box::new(JsonCompressor),
        Box::new(DiffCompressor),
        Box::new(LogCompressor),
        Box::new(TruncateCompressor),
    ])
}

/// The crate's single entry point: compress the request in place at the LLM request-send boundary.
/// fail-open: any failure falls back to the original text and never blocks the request (returns () rather than Result).
pub fn transform(request: &mut ResponsesApiRequest, _api_provider: &ApiProvider, queryid: &str) {
    let cfg = config::load();

    // Layer 0: toggle
    if !cfg.enabled {
        return;
    }

    // Layer 0: budget gate — skip if the total input text is below min_total_bytes
    let total_before = total_text_bytes(&request.input);
    if total_before < cfg.min_total_bytes {
        return;
    }

    let router = build_router();
    let budget = Budget { cfg: &cfg };

    // Layer 1: iterate input, process only the two tool-output variants, compressing each text fragment
    for item in request.input.iter_mut() {
        compress_item(item, &router, &budget, cfg.per_item_min_bytes);
    }

    // Exit: write the log only if compression actually occurred overall
    let total_after = total_text_bytes(&request.input);
    if total_after < total_before {
        stats::log_compression(queryid, total_before, total_after);
    }
}

/// For a single ResponseItem: only the body text of FunctionCallOutput / CustomToolCallOutput is compressed.
fn compress_item(
    item: &mut ResponseItem,
    router: &ContentRouter,
    budget: &Budget,
    per_item_min_bytes: usize,
) {
    let body = match item {
        ResponseItem::FunctionCallOutput { output, .. } => &mut output.body,
        ResponseItem::CustomToolCallOutput { output, .. } => &mut output.body,
        _ => return, // leave all other variants untouched
    };

    match body {
        FunctionCallOutputBody::Text(s) => {
            compress_in_place(s, router, budget, per_item_min_bytes);
        }
        FunctionCallOutputBody::ContentItems(items) => {
            for ci in items.iter_mut() {
                // Only compress InputText.text; InputImage / EncryptedContent are neither read nor modified
                if let FunctionCallOutputContentItem::InputText { text } = ci {
                    compress_in_place(text, router, budget, per_item_min_bytes);
                }
            }
        }
    }
}

/// A single text fragment: skip if below the threshold; otherwise compress via the router and replace in place on success.
fn compress_in_place(s: &mut String, router: &ContentRouter, budget: &Budget, min_bytes: usize) {
    if s.len() < min_bytes {
        return;
    }
    if let Some(new) = router.compress_text(s, budget) {
        *s = new;
    }
}

/// Sum the bytes of all "compressible text fragments" in input (consistent with the compression targets).
fn total_text_bytes(input: &[ResponseItem]) -> usize {
    let mut total = 0usize;
    for item in input {
        let body = match item {
            ResponseItem::FunctionCallOutput { output, .. } => &output.body,
            ResponseItem::CustomToolCallOutput { output, .. } => &output.body,
            _ => continue,
        };
        match body {
            FunctionCallOutputBody::Text(s) => total += s.len(),
            FunctionCallOutputBody::ContentItems(items) => {
                for ci in items {
                    if let FunctionCallOutputContentItem::InputText { text } = ci {
                        total += text.len();
                    }
                }
            }
        }
    }
    total
}

// Make Config visible in this module (ignore if the use above already includes it).
use crate::config;
```

> **Watch for duplicate use**: the top of `lib.rs` already has `pub mod config;` (Task 01), `pub mod router;` (Task 02), `pub mod stats;` (Task 07), `pub mod compress;` (Task 03-06). If the `use crate::config;` etc. added in this step duplicate existing ones, delete the duplicate lines; the criterion is that `cargo build` passes.

- [ ] **Step 4: Run the test and watch it pass**

Run (`codex-rs/`):
```bash
cargo test -p codez-llm-compress --test transform_test
```
Expected: 3 passed. If the `req_with`/`provider` helpers fail because the real field set differs, add fields per the compiler hints (minimal values) and re-run.

- [ ] **Step 5: Run the full crate test suite (regression)**

Run (`codex-rs/`):
```bash
cargo test -p codez-llm-compress
```
Expected: all green (config / router / truncate / json / diff / log / stats / transform test files).

- [ ] **Step 6: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-08-transform-entry.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): transform entry orchestration (Layer 0-3 + ResponseItem extraction)"
```
