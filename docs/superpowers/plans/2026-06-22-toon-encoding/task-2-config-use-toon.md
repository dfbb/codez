# Task 2: Add `use_toon` config switch

> Self-contained task. Read `00-overview.md` for global context and constraints.
> Build/test inside `codex-rs/`; test command `cargo nextest run -p codez-llm-compress`.

**Goal:** Add the `use_toon` master switch to `JsonCfg`, defaulting to `true`,
using the existing container-level `#[serde(default)]` + `Default` impl pattern.
This task is purely additive — it does NOT remove `csv_schema` or `TabularCfg`
yet (those die in Task 5), so the crate keeps compiling between tasks.

**Files:**
- Modify: `zmod/llm-compress/src/config.rs` (`JsonCfg` struct + its `Default`)
- Test: `zmod/llm-compress/tests/config_test.rs` (add one test)

**Interfaces:**
- Consumes: nothing.
- Produces, relied on by Tasks 3 & 4:
  - `cfg.json.use_toon: bool` — defaults to `true`. Master switch gating both
    `JsonCompressor` and `TabularCompressor` TOON encoding.

---

- [ ] **Step 1: Write the failing test**

Add to `zmod/llm-compress/tests/config_test.rs`:

```rust
#[test]
fn use_toon_defaults_true_and_parses_false() {
    // Default (field absent) must be true.
    let cfg = Config::disabled();
    assert!(cfg.json.use_toon, "use_toon must default to true");

    // Explicit false in config must parse as false.
    let f = write_tmp(
        "[llm_compress]\nenabled = true\n\n[llm_compress.json]\nuse_toon = false\n",
    );
    let parsed = load_from(f.path());
    assert!(!parsed.json.use_toon, "use_toon = false must parse");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(use_toon_defaults_true_and_parses_false)'
```

Expected: FAIL — `JsonCfg` has no field `use_toon` (compile error).

- [ ] **Step 3: Add the field**

In `zmod/llm-compress/src/config.rs`, the struct currently is:

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JsonCfg {
    pub csv_schema: bool,
}
```

Add the field (keep `csv_schema` for now — removed in Task 5):

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JsonCfg {
    pub csv_schema: bool,
    /// Master switch for TOON encoding (JsonCompressor + TabularCompressor).
    /// Default true. Container-level `#[serde(default)]` + the Default impl
    /// below supply the true default — do NOT use a field-level
    /// `#[serde(default)]`, which would default bool to false.
    pub use_toon: bool,
}
```

- [ ] **Step 4: Set the default to true**

The `Default` impl currently is:

```rust
impl Default for JsonCfg {
    fn default() -> Self {
        Self { csv_schema: true }
    }
}
```

Change it to:

```rust
impl Default for JsonCfg {
    fn default() -> Self {
        Self { csv_schema: true, use_toon: true }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(use_toon_defaults_true_and_parses_false)'
```

Expected: PASS.

- [ ] **Step 6: Run the full suite (nothing else should break)**

Run:

```bash
cargo nextest run -p codez-llm-compress
```

Expected: all PASS (purely additive field).

- [ ] **Step 7: Commit**

```bash
git add zmod/llm-compress/src/config.rs zmod/llm-compress/tests/config_test.rs
git commit -m "feat(llm-compress): add json.use_toon switch (default true)"
```
