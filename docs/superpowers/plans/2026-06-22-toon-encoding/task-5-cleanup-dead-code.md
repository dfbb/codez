# Task 5: Remove dead csv-schema / TabularCfg code

> Self-contained task. Read `00-overview.md` for global context and constraints.
> Build/test inside `codex-rs/`; test command `cargo nextest run -p codez-llm-compress`.

**Goal:** Delete everything left orphaned by Tasks 3 & 4: the `_schema`/`_rows`
module (`schema.rs`) and its test, the `csv_schema` config field, and the entire
`TabularCfg` (now that Tabular is gated by `json.use_toon`). After this task the
crate has no references to the old csv-schema path.

**Files:**
- Delete: `zmod/llm-compress/src/compress/schema.rs`
- Delete: `zmod/llm-compress/tests/schema_test.rs`
- Modify: `zmod/llm-compress/src/compress/mod.rs` (remove `pub mod schema;`)
- Modify: `zmod/llm-compress/src/config.rs` (remove `csv_schema`, `TabularCfg`,
  the `tabular` field, and their defaults)
- Modify: `zmod/llm-compress/tests/config_test.rs` (drop `csv_schema` assertions)

**Interfaces:**
- Consumes: nothing new.
- Produces: a crate with no `csv_schema` / `TabularCfg` / `schema` symbols.

**Precondition check:** This task assumes Tasks 3 & 4 already removed all
`to_schema_form` / `csv_schema` / `cfg.tabular` references from `json.rs` and
`tabular.rs`. Before starting, confirm:

```bash
grep -rn "to_schema_form\|csv_schema\|cfg\.tabular\|\.tabular\.enabled" \
  zmod/llm-compress/src/compress/json.rs zmod/llm-compress/src/compress/tabular.rs
```

Expected: no output. If anything prints, the prior task is incomplete — stop and
flag it; do not paper over it here.

---

- [ ] **Step 1: Delete the schema module and its test**

```bash
git rm zmod/llm-compress/src/compress/schema.rs \
       zmod/llm-compress/tests/schema_test.rs
```

- [ ] **Step 2: Remove the module declaration**

In `zmod/llm-compress/src/compress/mod.rs`, delete this line:

```rust
pub mod schema;
```

- [ ] **Step 3: Remove `csv_schema` from `JsonCfg`**

In `zmod/llm-compress/src/config.rs`, the struct (after Task 2) is:

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JsonCfg {
    pub csv_schema: bool,
    /// Master switch for TOON encoding (JsonCompressor + TabularCompressor).
    /// Default true. ...
    pub use_toon: bool,
}
```

Remove the `csv_schema` field so it reads:

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JsonCfg {
    /// Master switch for TOON encoding (JsonCompressor + TabularCompressor).
    /// Default true. Container-level `#[serde(default)]` + the Default impl
    /// supply the true default.
    pub use_toon: bool,
}
```

And its `Default` impl, currently:

```rust
impl Default for JsonCfg {
    fn default() -> Self {
        Self { csv_schema: true, use_toon: true }
    }
}
```

becomes:

```rust
impl Default for JsonCfg {
    fn default() -> Self {
        Self { use_toon: true }
    }
}
```

- [ ] **Step 4: Remove `TabularCfg` and the `tabular` field**

In `zmod/llm-compress/src/config.rs`:

(a) In the top-level `Config` struct, remove the line:

```rust
    pub tabular: TabularCfg,
```

(b) In `impl Default for Config`, remove the line:

```rust
            tabular: TabularCfg::default(),
```

(c) Delete the `TabularCfg` struct entirely:

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TabularCfg {
    pub enabled: bool,
}
```

(d) Delete its `Default` impl:

```rust
impl Default for TabularCfg {
    fn default() -> Self {
        Self { enabled: true }
    }
}
```

- [ ] **Step 5: Fix `config_test.rs`**

In `zmod/llm-compress/tests/config_test.rs`, the `enabled_section_overrides_defaults`
test writes a `[llm_compress.json]` section with `csv_schema = false` and asserts
it. Remove the `csv_schema` line from the embedded TOML and the assertion.

The embedded config string currently contains:

```
[llm_compress.json]
csv_schema = false
```

Change it to exercise the surviving field instead:

```
[llm_compress.json]
use_toon = false
```

And the assertion currently is:

```rust
    assert!(!cfg.json.csv_schema);
```

Change it to:

```rust
    assert!(!cfg.json.use_toon);
```

(The dedicated `use_toon_defaults_true_and_parses_false` test from Task 2 stays
as the focused coverage; this edit just keeps the broader override test
compiling against the new field.)

- [ ] **Step 6: Build to confirm no dangling references**

Run:

```bash
cargo build -p codez-llm-compress
```

Expected: builds clean. A compile error here means a reference to a removed
symbol remains — fix the reference (it will point you to the file/line).

- [ ] **Step 7: Run the full suite**

Run:

```bash
cargo nextest run -p codez-llm-compress
```

Expected: ALL tests PASS. This is the final green bar for the feature.

- [ ] **Step 8: Note (do not act) — stale reference fixtures**

`tests/fixtures/inherited/json/*.expected` are old `_schema`/`_rows` reference
outputs. The parity test only compares them for *lossy* products, and TOON is
lossless, so that branch is skipped and the stale files cause no failure. Leave
them; reconciling reference fixtures is out of scope for this plan. If a reviewer
wants them refreshed, that is a separate task.

- [ ] **Step 9: Commit**

```bash
git add zmod/llm-compress/src/compress/mod.rs \
        zmod/llm-compress/src/config.rs \
        zmod/llm-compress/tests/config_test.rs
git commit -m "refactor(llm-compress): remove dead csv-schema module, csv_schema field, and TabularCfg"
```

(The `git rm` in Step 1 already staged the two deletions; this commit captures
the rest. If you prefer one commit, run Step 1's `git rm` and then this single
commit at the end.)
