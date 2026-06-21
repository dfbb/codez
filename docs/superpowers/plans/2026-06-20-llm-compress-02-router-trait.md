# Task 02: Compressor trait + Budget + ContentRouter

> Part of `2026-06-20-llm-compress-00-index.md`. Read the index before starting. Depends on Task 01.

**Goal:** Define the public contract for compressors: the `Compressor` trait, `Budget` (carrying each compressor's config slice), `CompressOutcome`, and `ContentRouter` (fixed-priority detect→compress, with fail-open `catch_unwind`). This task validates router orchestration and fail-open behavior with a built-in `NoopCompressor`; the real compressors are implemented in 03-06.

**Spec coverage:** §4 (two-layer pipeline / trait / ContentRouter / fail-open).

**Files:**
- Create: `zmod/llm-compress/src/router.rs`
- Modify: `zmod/llm-compress/src/lib.rs` (add `pub mod router;`)
- Test: `zmod/llm-compress/tests/router_test.rs`

**Interfaces:**
- Consumes (from Task 01): `config::{Config, TruncateCfg, JsonCfg, DiffCfg, LogCfg}`.
- Produces (03-06 and 08 depend on these **pinned** signatures):
  - `pub struct Budget<'a> { pub cfg: &'a Config }` — compressors read their own config slice from it (e.g. `budget.cfg.truncate`).
  - `pub enum CompressOutcome { Compressed { text: String, saved_bytes: usize }, Unchanged }`
  - `pub trait Compressor: Send + Sync { fn name(&self) -> &'static str; fn detect(&self, text: &str) -> bool; fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome; }`
  - `pub struct ContentRouter { compressors: Vec<Box<dyn Compressor>> }`
    - `pub fn new(compressors: Vec<Box<dyn Compressor>>) -> Self`
    - `pub fn compress_text(&self, text: &str, budget: &Budget) -> Option<String>` — runs `compress` on the first compressor whose `detect` matches, in order (wrapped in `catch_unwind`); returns `Some(new)` only when compression actually happened (`Compressed` with `saved_bytes>0`); `Unchanged` / no match / panic → `None` (the caller keeps the original text).

---

- [ ] **Step 1: Write the failing test**

Create `zmod/llm-compress/tests/router_test.rs`:

```rust
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentRouter};

/// Fake compressor that claims everything and replaces the text with a fixed short string.
struct HalfCompressor;
impl Compressor for HalfCompressor {
    fn name(&self) -> &'static str { "half" }
    fn detect(&self, _t: &str) -> bool { true }
    fn compress(&self, text: &str, _b: &Budget) -> CompressOutcome {
        let new = format!("[half]{}", &text[..text.len() / 2]);
        let saved = text.len().saturating_sub(new.len());
        if saved > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes: saved }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// Never claims.
struct NeverCompressor;
impl Compressor for NeverCompressor {
    fn name(&self) -> &'static str { "never" }
    fn detect(&self, _t: &str) -> bool { false }
    fn compress(&self, _t: &str, _b: &Budget) -> CompressOutcome { CompressOutcome::Unchanged }
}

/// detect matches but compress panics —— validates fail-open.
struct PanicCompressor;
impl Compressor for PanicCompressor {
    fn name(&self) -> &'static str { "panic" }
    fn detect(&self, _t: &str) -> bool { true }
    fn compress(&self, _t: &str, _b: &Budget) -> CompressOutcome { panic!("boom") }
}

fn budget(cfg: &Config) -> Budget<'_> { Budget { cfg } }

#[test]
fn first_detecting_compressor_wins() {
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(NeverCompressor), Box::new(HalfCompressor)]);
    let input = "0123456789ABCDEF"; // 16 bytes
    let out = r.compress_text(input, &budget(&cfg));
    assert!(out.is_some());
    assert!(out.unwrap().starts_with("[half]"));
}

#[test]
fn no_detect_returns_none() {
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(NeverCompressor)]);
    assert!(r.compress_text("anything", &budget(&cfg)).is_none());
}

#[test]
fn panic_in_compress_is_caught_and_returns_none() {
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(PanicCompressor)]);
    // Must not panic out; returns None so the caller keeps the original text.
    let out = r.compress_text("some payload text", &budget(&cfg));
    assert!(out.is_none());
}

#[test]
fn unchanged_outcome_returns_none() {
    struct Claims;
    impl Compressor for Claims {
        fn name(&self) -> &'static str { "claims" }
        fn detect(&self, _t: &str) -> bool { true }
        fn compress(&self, _t: &str, _b: &Budget) -> CompressOutcome { CompressOutcome::Unchanged }
    }
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(Claims)]);
    assert!(r.compress_text("xxxx", &budget(&cfg)).is_none());
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run (`codex-rs/`):
```bash
cargo test -p codez-llm-compress --test router_test
```
Expected: compile error (the `router` module/types are undefined).

- [ ] **Step 3: Write router.rs**

Create `zmod/llm-compress/src/router.rs`:

```rust
//! Compressor public contract + ContentRouter (fixed priority + fail-open).

use crate::config::Config;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Compressors read their own config slice from it (e.g. budget.cfg.truncate).
pub struct Budget<'a> {
    pub cfg: &'a Config,
}

/// The result of a single compressor processing a piece of text.
pub enum CompressOutcome {
    Compressed { text: String, saved_bytes: usize },
    Unchanged,
}

/// Content detection + compression. Implementers guarantee detect is cheap and compress does not rely on external mutable state.
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}

/// Fixed-priority routing: the first compressor whose detect matches handles compression; compress is wrapped in catch_unwind for fail-open.
pub struct ContentRouter {
    compressors: Vec<Box<dyn Compressor>>,
}

impl ContentRouter {
    pub fn new(compressors: Vec<Box<dyn Compressor>>) -> Self {
        Self { compressors }
    }

    /// Returns Some(new) only when compression actually happened (Compressed with saved_bytes>0);
    /// Unchanged / no match / panic → None (the caller keeps the original text).
    pub fn compress_text(&self, text: &str, budget: &Budget) -> Option<String> {
        let c = self.compressors.iter().find(|c| {
            // detect also catches panics: a faulty compressor is treated as not claiming.
            catch_unwind(AssertUnwindSafe(|| c.detect(text))).unwrap_or(false)
        })?;

        let outcome = catch_unwind(AssertUnwindSafe(|| c.compress(text, budget)));
        match outcome {
            Ok(CompressOutcome::Compressed { text: new, saved_bytes }) if saved_bytes > 0 => {
                Some(new)
            }
            Ok(_) => None,
            Err(_) => {
                tracing::warn!("llm-compress: compressor '{}' panicked, passing through", c.name());
                None
            }
        }
    }
}
```

- [ ] **Step 4: Register the module in lib.rs**

Modify `zmod/llm-compress/src/lib.rs`, adding after `pub mod config;`:

```rust
pub mod router;
```

- [ ] **Step 5: Run the test and watch it pass**

Run (`codex-rs/`):
```bash
cargo test -p codez-llm-compress --test router_test
```
Expected: `test result: ok. 4 passed`.

- [ ] **Step 6: Commit**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-02-router-trait.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): Compressor trait + Budget + ContentRouter with fail-open"
```
