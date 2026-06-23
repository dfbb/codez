# Task 3: Rewrite `JsonCompressor` to emit TOON

> Self-contained task. Read `00-overview.md` for global context and constraints.
> Build/test inside `codex-rs/`; test command `cargo nextest run -p codez-llm-compress`.

**Goal:** Replace `JsonCompressor`'s `_schema`/`_rows` + RLE logic with TOON
encoding via the Task 1 helper. The compressor now parses input to a `Value`,
calls `encode_checked`, and claims only when all five conditions hold. Product is
`ContentKind::Toon`, `lossy=false`.

**Files:**
- Modify (full rewrite of the impl): `zmod/llm-compress/src/compress/json.rs`
- Test (rewrite): `zmod/llm-compress/tests/json_test.rs`
- Modify: `zmod/llm-compress/tests/parity_test.rs:75-82` (TOON output is not JSON)

**Interfaces:**
- Consumes (from Task 1): `crate::compress::toon::encode_checked(&Value) -> Option<String>`,
  `crate::router::ContentKind::Toon`.
- Consumes (from Task 2): `budget.cfg.json.use_toon: bool`.
- Produces: `JsonCompressor` with `name() == "json"`, claiming object/array JSON
  and emitting TOON. (Same public type, new behavior.)

---

- [ ] **Step 1: Rewrite the test file**

Replace the entire contents of `zmod/llm-compress/tests/json_test.rs` with:

```rust
use codez_llm_compress::compress::json::JsonCompressor;
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::Value;

fn budget(cfg: &Config) -> Budget<'_> {
    Budget { cfg, cmd: None }
}

#[test]
fn detect_accepts_object_and_array_rejects_scalar_and_garbage() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000; // large enough, no yield to Truncate
    let c = JsonCompressor;
    let b = budget(&cfg);
    // Object/array that shrink under TOON are claimed.
    assert!(c.detect(r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"}]"#, &b));
    // Scalars are never claimed.
    assert!(!c.detect("\"a quoted string\"", &b));
    assert!(!c.detect("123", &b));
    // Non-JSON.
    assert!(!c.detect("not json {", &b));
    assert!(!c.detect("{unquoted: key}", &b));
    assert!(!c.detect("", &b));
}

#[test]
fn compress_emits_round_trippable_toon_for_homogeneous_array() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"},{"id":3,"name":"carol"}]"#;
    let CompressOutcome::Compressed { text: new, lossy, kind, saved_bytes } =
        c.compress(text, &budget(&cfg))
    else {
        panic!("expected Compressed");
    };
    assert!(!lossy, "TOON is lossless");
    assert_eq!(kind, ContentKind::Toon);
    assert_eq!(saved_bytes, text.len() - new.len());
    // Round-trips back to the original value.
    let back: Value = toon_format::decode_default(&new).unwrap();
    assert_eq!(back, serde_json::from_str::<Value>(text).unwrap());
    // Tabular header present (uniform object array).
    assert!(new.contains("{id,name}:"), "got: {new:?}");
}

#[test]
fn detect_yields_to_truncate_when_toon_exceeds_max_bytes() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 20; // tiny
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"},{"id":3,"name":"carol"}]"#;
    assert!(!c.detect(text, &budget(&cfg)), "TOON over max_bytes → yield to Truncate");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

#[test]
fn detect_false_when_toon_not_smaller() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    // A tiny object whose TOON form is not strictly smaller than the input.
    let text = r#"{"a":1}"#;
    assert!(!c.detect(text, &budget(&cfg)), "no size benefit → not claimed");
}

#[test]
fn use_toon_false_disables_claim() {
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    cfg.json.use_toon = false;
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"}]"#;
    assert!(!c.detect(text, &budget(&cfg)), "use_toon=false → detect false");
    assert!(matches!(c.compress(text, &budget(&cfg)), CompressOutcome::Unchanged));
}

#[test]
fn toon_output_is_deterministic_across_runs() {
    // Cache stability: compression must be a pure function of content.
    // Encoding the same input many times must yield byte-identical TOON.
    let mut cfg = Config::disabled();
    cfg.truncate.max_bytes = 100_000;
    let c = JsonCompressor;
    let text = r#"[{"id":1,"name":"alice","tags":["x","y"]},{"id":2,"name":"bob","tags":["z"]}]"#;
    let first = match c.compress(text, &budget(&cfg)) {
        CompressOutcome::Compressed { text, .. } => text,
        CompressOutcome::Unchanged => panic!("expected Compressed"),
    };
    for _ in 0..20 {
        let CompressOutcome::Compressed { text: again, .. } = c.compress(text, &budget(&cfg))
        else {
            panic!("expected Compressed");
        };
        assert_eq!(again, first, "TOON output must be byte-identical across runs");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'package(codez-llm-compress) and test(/json_test/)'
```

Expected: FAIL — old `JsonCompressor` returns `ContentKind::Json`, not `Toon`,
and still references RLE behavior; new assertions do not hold.

- [ ] **Step 3: Rewrite `json.rs`**

Replace the entire contents of `zmod/llm-compress/src/compress/json.rs` with:

```rust
//! JsonCompressor — encodes object/array JSON tool-output as TOON.
//! Product is kind=Toon, lossy=false. Claims only when the TOON form passes
//! the round-trip self-check, is strictly smaller than the input, and fits
//! within truncate.max_bytes; otherwise yields to the next compressor
//! (ultimately Truncate). detect and compress share `try_toon` so they agree.

use crate::compress::toon::encode_checked;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::Value;

pub struct JsonCompressor;

/// Run the full claim pipeline. Returns Some(toon) iff:
///   use_toon, parses to object/array, encodes + round-trips,
///   toon.len() < text.len(), and toon.len() <= truncate.max_bytes.
fn try_toon(text: &str, budget: &Budget) -> Option<String> {
    if !budget.cfg.json.use_toon {
        return None;
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(v @ Value::Object(_)) | Ok(v @ Value::Array(_)) => v,
        _ => return None,
    };
    let toon = encode_checked(&value)?;
    if toon.len() < text.len() && toon.len() <= budget.cfg.truncate.max_bytes {
        Some(toon)
    } else {
        None
    }
}

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    fn detect(&self, text: &str, budget: &Budget) -> bool {
        try_toon(text, budget).is_some()
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        match try_toon(text, budget) {
            Some(toon) => {
                let saved = text.len().saturating_sub(toon.len());
                if saved == 0 {
                    return CompressOutcome::Unchanged;
                }
                CompressOutcome::Compressed {
                    text: toon,
                    saved_bytes: saved,
                    lossy: false,
                    kind: ContentKind::Toon,
                }
            }
            None => CompressOutcome::Unchanged,
        }
    }
}
```

- [ ] **Step 4: Fix the parity test's JSON-parseable assertion**

`tests/parity_test.rs` asserts that `json`/`tabular` compressor output parses as
JSON. TOON is not JSON, so this assertion is now wrong. The block currently is:

```rust
        // 硬不变量 3:JSON 压缩器产物可 parse
        if fx.compressor == "json" || fx.compressor == "tabular" {
            serde_json::from_str::<serde_json::Value>(&out)
                .unwrap_or_else(|_| panic!("[{}] JSON 产物必须可 parse", fx.file));
        }
```

Replace it with a TOON round-trip invariant:

```rust
        // Hard invariant 3: json/tabular compressors emit round-trippable TOON.
        if fx.compressor == "json" || fx.compressor == "tabular" {
            toon_format::decode_default::<serde_json::Value>(&out)
                .unwrap_or_else(|_| panic!("[{}] TOON product must decode", fx.file));
        }
```

(`toon-format` is a normal dependency, so it is available to integration tests
without any `Cargo.toml` change.)

- [ ] **Step 5: Run the json + parity tests to verify they pass**

Run:

```bash
cargo nextest run -p codez-llm-compress -E 'test(/json_test/) + test(parity_invariants_hold_for_all_fixtures)'
```

Expected: PASS. Note: the JSON fixtures' reference `.expected` files were
`_schema`/`_rows`; the parity test only compares *volume for lossy products* and
our TOON output is lossless, so the `lossy && ref_output` branch is skipped — no
fixture content update needed here. (Fixtures are fully reconciled in Task 5.)

- [ ] **Step 6: Run the full suite**

Run:

```bash
cargo nextest run -p codez-llm-compress
```

Expected: the only remaining failures, if any, are in `schema_test.rs` /
`tabular_test.rs` / `config_test.rs` which still reference the old paths — those
are owned by Tasks 4 and 5. `json_test`, `parity`, `toon_helper`, and
orchestration tests must pass. If an orchestration test that routed through
`JsonCompressor` now fails because output is TOON not JSON, note it for Task 4
(which owns the orchestrator CCR change) — do not patch it here.

- [ ] **Step 7: Commit**

```bash
git add zmod/llm-compress/src/compress/json.rs \
        zmod/llm-compress/tests/json_test.rs \
        zmod/llm-compress/tests/parity_test.rs
git commit -m "feat(llm-compress): JsonCompressor emits TOON (kind=Toon), drop RLE/csv-schema path"
```
