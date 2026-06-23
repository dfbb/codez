# Task 1: Add toon-format dependency, `ContentKind::Toon`, and the shared TOON helper

> Self-contained task. Read `00-overview.md` for global context and constraints.
> Implement in numeric order. Build/test inside `codex-rs/` with the dev symlink
> in place; test command `cargo nextest run -p codez-llm-compress`.

**Goal:** Land the foundations every later task depends on: the `toon-format`
dependency, a new `ContentKind::Toon` router variant, and a single
`compress/toon.rs` helper that encodes a `serde_json::Value` to TOON and runs the
mandatory round-trip self-check.

**Files:**
- Modify: `zmod/llm-compress/Cargo.toml` (add dependency)
- Modify: `zmod/llm-compress/src/router.rs:9-13` (add `Toon` to `ContentKind`)
- Create: `zmod/llm-compress/src/compress/toon.rs` (the helper)
- Modify: `zmod/llm-compress/src/compress/mod.rs` (add `pub mod toon;`)
- Test: `zmod/llm-compress/tests/toon_helper_test.rs` (new)

**Interfaces:**
- Consumes: nothing (first task).
- Produces, relied on by Tasks 3 & 4:
  - `crate::router::ContentKind::Toon` — variant, always paired with `lossy=false`.
  - `crate::compress::toon::encode_checked(value: &serde_json::Value) -> Option<String>`
    — returns `Some(toon)` iff `encode_default` succeeds **and**
    `decode_default::<Value>(&toon) == *value`; otherwise `None`.

---

- [ ] **Step 1: Add the dependency**

Add to `zmod/llm-compress/Cargo.toml` under `[dependencies]` (exact line — the
default `cli` feature must NOT be enabled):

```toml
toon-format = { version = "0.5", default-features = false }
```

- [ ] **Step 2: Verify it resolves and builds (no code using it yet)**

Run (inside `codex-rs/`, dev symlink in place):

```bash
cargo build -p codez-llm-compress
```

Expected: builds clean. If the registry resolves a newer 0.5.x patch, that is
fine (caret `0.5`). If it fails to resolve, confirm network/registry access — do
NOT switch to a git dependency.

- [ ] **Step 3: Add the `Toon` variant to `ContentKind`**

In `zmod/llm-compress/src/router.rs`, the enum currently is:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Json,
}
```

Change it to:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Json,
    /// TOON product (Token-Oriented Object Notation). Not JSON; always
    /// lossless (`lossy=false`). The orchestrator treats it like `Json`:
    /// never appends a CCR pointer, and never re-parses it as JSON.
    Toon,
}
```

- [ ] **Step 4: Write the failing test for the helper**

Create `zmod/llm-compress/tests/toon_helper_test.rs`:

```rust
use codez_llm_compress::compress::toon::encode_checked;
use serde_json::json;

#[test]
fn encodes_homogeneous_array_and_round_trips() {
    let v = json!([{"id":1,"name":"alice"},{"id":2,"name":"bob"}]);
    let toon = encode_checked(&v).expect("homogeneous array encodes + round-trips");
    // TOON tabular header for an array of uniform objects.
    assert!(toon.contains("[2]{id,name}:"), "got: {toon:?}");
    assert!(toon.len() < v.to_string().len());
}

#[test]
fn encodes_nested_object_and_round_trips() {
    let v = json!({"list":[1,2,3],"nested":{"a":{"b":1}},"name":"keep"});
    let toon = encode_checked(&v).expect("nested object encodes + round-trips");
    // Re-decoding must reproduce the original value exactly.
    let back: serde_json::Value = toon_format::decode_default(&toon).unwrap();
    assert_eq!(back, v);
}

#[test]
fn rejects_when_round_trip_loses_information() {
    // Float 1.0 encodes as "1" in TOON and decodes back as integer 1,
    // so the round-trip self-check must FAIL and return None (fall back).
    let v = json!({"x": 1.0});
    assert!(encode_checked(&v).is_none(), "lossy round-trip must be rejected");
}

#[test]
fn preserves_ambiguous_strings() {
    // Strings that look like numbers/bools must survive (TOON quotes them).
    let v = json!({"code":"007","flag":"true"});
    let toon = encode_checked(&v).expect("ambiguous strings round-trip via quoting");
    let back: serde_json::Value = toon_format::decode_default(&toon).unwrap();
    assert_eq!(back, v);
}
```

- [ ] **Step 5: Run the test to verify it fails**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(/toon_helper/)'
```

Expected: FAIL — `compress::toon` module / `encode_checked` does not exist yet
(compile error).

- [ ] **Step 6: Implement the helper**

Create `zmod/llm-compress/src/compress/toon.rs`:

```rust
//! Shared TOON encoding helper. Encodes a JSON `Value` to TOON and enforces
//! the mandatory round-trip self-check: a product is returned ONLY if decoding
//! it back yields a value byte-for-byte equal to the input. TOON is the model's
//! only view of the tool output, so a non-round-trippable encoding must be
//! discarded (fail-open) rather than written back.

use serde_json::Value;

/// Encode `value` to TOON. Returns `Some(toon)` iff encoding succeeds AND
/// `decode_default::<Value>(&toon) == *value`. Any encode error, decode error,
/// or inequality → `None` (caller falls back to the original text).
pub fn encode_checked(value: &Value) -> Option<String> {
    let toon = toon_format::encode_default(value).ok()?;
    let back: Value = toon_format::decode_default(&toon).ok()?;
    if back == *value {
        Some(toon)
    } else {
        None
    }
}
```

- [ ] **Step 7: Register the module**

In `zmod/llm-compress/src/compress/mod.rs`, add a line (keep existing lines):

```rust
pub mod toon;
```

- [ ] **Step 8: Run the helper test to verify it passes**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(/toon_helper/)'
```

Expected: PASS (all four tests).

- [ ] **Step 9: Run the full crate suite (preserve_order regression check)**

`toon-format` enables `serde_json/preserve_order` workspace-wide. Confirm nothing
in our crate depended on sorted-key Map iteration.

Run:

```bash
cargo nextest run -p codez-llm-compress
```

Expected: all existing tests still PASS (we have not changed compressor behavior
yet — only added an enum variant and a new module). If a test now fails purely
due to key ordering, note it; it will be reconciled when its compressor is
rewritten in Tasks 3–5. Do not "fix" unrelated tests here.

- [ ] **Step 10: Commit**

```bash
git add zmod/llm-compress/Cargo.toml \
        zmod/llm-compress/src/router.rs \
        zmod/llm-compress/src/compress/toon.rs \
        zmod/llm-compress/src/compress/mod.rs \
        zmod/llm-compress/tests/toon_helper_test.rs
git commit -m "feat(llm-compress): add toon-format dep, ContentKind::Toon, encode_checked helper"
```
