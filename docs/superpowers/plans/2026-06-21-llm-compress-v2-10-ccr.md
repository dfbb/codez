# Task 10 — ccr.rs: Persistence + Text Placeholder + sanitize + Dual Limits

> Belongs to `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §4.7. Depends on Task 01 (CcrCfg). Can run in parallel with 05–09 (independent module); wired in during Task 11 orchestration.

**Goal:** Implement CCR: `RequestCtx` + `CcrRegistry` (one file per fragment) + `attach` (persist the fragment's original text to disk, and per the core principle either produce a Text placeholder or return the original text). sanitize path components (guard against `/`, `..`, excessive length); dual-limit cleanup (file count + total thread bytes); single file over limit / persistence failure → return the original text (preserve "anything lossy must be retrievable").

## Files
- Create: `zmod/llm-compress/src/ccr.rs`
- Modify: `zmod/llm-compress/src/lib.rs` (add `pub mod ccr;`)
- Test: `zmod/llm-compress/tests/ccr_test.rs`

**Interfaces:**
- Consumes: Task 01's `config::CcrCfg`; `sha2` (dependency already added in Task 01).
- Produces:
  - `pub struct RequestCtx<'a> { pub queryid: &'a str, pub query_terms: Vec<String>, pub cmd_index: std::collections::HashMap<String, crate::command::CommandHint>, pub ccr: std::cell::RefCell<CcrRegistry> }`
  - `pub struct CcrRegistry { ... }` (internally records (call_id,fragment_hash)→path, empty by default; `CcrRegistry::new()`)
  - `pub fn ccr::attach(compressed: String, original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> String`
  - `pub fn ccr::ccr_root() -> Option<std::path::PathBuf>` (`~/.codex/llm-compress/ccr`, for test injection)

> **Core principle (spec §4.7, hardcoded):** Under `enabled=true`, attach has only two outcomes — successful persistence → "lossy product + Text placeholder containing the path"; any inability to persist (disk failure / over max_file_bytes / abnormal path) → **return the original text** (give up on compression). `enabled=false` → no persistence, no path appended, return the passed-in compressed value (preserving the compressor's own elision placeholder).

---

- [ ] **Step 1: Write failing tests (inject a temp directory via HOME, following the stats_test pattern)**

Create `zmod/llm-compress/tests/ccr_test.rs`:

```rust
use codez_llm_compress::ccr::{attach, CcrRegistry, RequestCtx};
use codez_llm_compress::config::CcrCfg;
use std::cell::RefCell;
use std::collections::HashMap;

fn ctx<'a>(queryid: &'a str) -> RequestCtx<'a> {
    RequestCtx {
        queryid,
        query_terms: Vec::new(),
        cmd_index: HashMap::new(),
        ccr: RefCell::new(CcrRegistry::new()),
    }
}

fn cfg_enabled() -> CcrCfg {
    CcrCfg { enabled: true, max_files_per_thread: 200, max_thread_bytes: 67_108_864, max_file_bytes: 4_194_304 }
}

#[test]
fn enabled_writes_file_and_appends_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-abc");
    let original = "VERY LONG ORIGINAL CONTENT ".repeat(50);
    let compressed = "[llm-compress: 略 49 行]".to_string();
    let out = attach(compressed.clone(), &original, &c, "call1", &cfg_enabled());
    // the placeholder has the original-text path appended
    assert!(out.contains("原文:"), "contains path placeholder");
    assert!(out.starts_with("[llm-compress: 略 49 行]"));
    // the file the path points to has contents == original text
    let path_part = out.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let written = std::fs::read_to_string(path_part).unwrap();
    assert_eq!(written, original);
}

#[test]
fn disabled_returns_compressed_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-x");
    let mut cfg = cfg_enabled();
    cfg.enabled = false;
    let compressed = "[llm-compress: 略 N 行]".to_string();
    let out = attach(compressed.clone(), "original", &c, "call1", &cfg);
    assert_eq!(out, compressed, "disabled: return compressed as-is, no path appended");
}

#[test]
fn over_max_file_bytes_returns_original() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-y");
    let mut cfg = cfg_enabled();
    cfg.max_file_bytes = 100;
    let original = "x".repeat(500); // > 100
    let compressed = "[llm-compress: 略]".to_string();
    let out = attach(compressed, &original, &c, "call1", &cfg);
    assert_eq!(out, original, "over max_file_bytes → return original text (keep lossy retrievable)");
}

#[test]
fn sanitizes_unsafe_path_components() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    // queryid contains / and .., call_id contains /
    let c = ctx("../../etc/evil");
    let original = "LONG CONTENT ".repeat(50);
    let out = attach("[llm-compress: 略]".to_string(), &original, &c, "a/b/../c", &cfg_enabled());
    let path_part = out.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    // the path must stay under HOME/.codex/llm-compress/ccr, with no traversal
    let root = tmp.path().join(".codex/llm-compress/ccr");
    let canon = std::fs::canonicalize(path_part).unwrap();
    assert!(canon.starts_with(std::fs::canonicalize(&root).unwrap()), "path must not traverse outside the ccr root");
}

#[test]
fn same_fragment_reuses_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-z");
    let original = "REPEATED CONTENT ".repeat(50);
    let o1 = attach("[c1]".to_string(), &original, &c, "call1", &cfg_enabled());
    let o2 = attach("[c2]".to_string(), &original, &c, "call1", &cfg_enabled());
    // same (call_id, fragment_hash) → same file path
    let p1 = o1.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let p2 = o2.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    assert_eq!(p1, p2);
}
```

> Note: tests inject via `std::env::set_var("HOME", ..)`. `tempfile` is already in dev-deps. These tests share the process environment variables; nextest runs each test in its own process by default, so it is safe; with `cargo test` thread concurrency they may interfere with each other — this crate's tests use `cargo test`, so the multiple HOME-mutating tests inside ccr_test **may conflict under concurrency**. Mitigation: each test uses a unique queryid subdirectory and asserts only against the file it wrote (already done above), with HOME pointing to its own tempdir (under concurrency, whichever HOME is set last takes effect, which can cause cross-talk). **If you observe flakiness during implementation**, add `use serial_test::serial;` at the top of ccr_test and annotate each test with `#[serial]` (requires adding `serial-test` to Cargo.toml dev-deps), or switch to an injectable variant of `ccr_root` (see the Step 2 alternative).

- [ ] **Step 2: Run and confirm failure**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test ccr_test 2>&1 | head`
Expected: FAIL (the `ccr` module does not exist)

- [ ] **Step 3: Implement ccr.rs (types + attach main logic)**

Create `zmod/llm-compress/src/ccr.rs`, starting with the types and attach:

```rust
//! CCR: for lossy compression, persist the fragment's original text to disk + write the path into a Text placeholder, so the model can retrieve it via shell/read (spec §4.7/E).
//! Core principle: under enabled, attach only produces a "placeholder containing the path" or "returns the original text", never "lossy without a path".

use crate::command::CommandHint;
use crate::config::CcrCfg;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// Context for a single request (constructed by Task 11 orchestration). Holds the mutable CCR registry.
pub struct RequestCtx<'a> {
    pub queryid: &'a str,
    pub query_terms: Vec<String>,
    pub cmd_index: HashMap<String, CommandHint>,
    pub ccr: RefCell<CcrRegistry>,
}

/// Records (call_id, fragment_hash) → already-persisted file path, to avoid persisting the same fragment twice.
#[derive(Default)]
pub struct CcrRegistry {
    written: HashMap<(String, String), PathBuf>,
}

impl CcrRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

/// CCR root directory ~/.codex/llm-compress/ccr. HOME unset → None.
pub fn ccr_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex").join("llm-compress").join("ccr"))
}

/// Persist the fragment's original text + append a Text retrieval placeholder. Called only for lossy=true items.
/// See spec §4.7 core principle: under enabled, either a "placeholder containing the path" or "return the original text".
pub fn attach(compressed: String, original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> String {
    if !cfg.enabled {
        return compressed; // disabled: keep the compressor's own placeholder, no path appended
    }
    // single file over limit → give up on compression, return original text (keep "anything lossy must be retrievable")
    if original.len() > cfg.max_file_bytes {
        return original.to_string();
    }
    match try_persist(original, ctx, call_id, cfg) {
        Some(path) => {
            let attached = format!("{compressed} [原文: {}]", path.display());
            // secondary size check: if the placeholder concatenation exceeds the original, fall back to a short reference; if still larger, return the original
            if attached.len() <= original.len() {
                attached
            } else {
                let short = format!("{compressed} [llm-compress: 见 ccr]");
                if short.len() <= original.len() {
                    short
                } else {
                    original.to_string()
                }
            }
        }
        None => original.to_string(), // persistence failed → return original text (don't leave an unretrievable lossy product behind)
    }
}
```

- [ ] **Step 4: Continue ccr.rs (try_persist + sanitize + dual limits)**

Append to the end of `ccr.rs`:

```rust
/// Persist: sanitize the path, run dual-limit cleanup, write the file. On success return the path; on any failure return None.
fn try_persist(original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> Option<PathBuf> {
    let frag_hash = short_hash(original);
    let key = (call_id.to_string(), frag_hash.clone());
    // same fragment already persisted → reuse
    if let Some(p) = ctx.ccr.borrow().written.get(&key) {
        return Some(p.clone());
    }
    let root = ccr_root()?;
    let thread_dir = root.join(sanitize_component(ctx.queryid, 64));
    if std::fs::create_dir_all(&thread_dir).is_err() {
        tracing::warn!("llm-compress: ccr mkdir failed");
        return None;
    }
    enforce_limits(&thread_dir, cfg);
    let fname = format!("{}-{}.txt", sanitize_component(call_id, 32), frag_hash);
    let path = thread_dir.join(fname);
    if std::fs::write(&path, original).is_err() {
        tracing::warn!("llm-compress: ccr write failed {path:?}");
        return None;
    }
    ctx.ccr.borrow_mut().written.insert(key, path.clone());
    Some(path)
}

/// First 12 hex chars of SHA256.
fn short_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Path-component sanitize: characters not in [A-Za-z0-9_-] → '_'; over max_len bytes → take the first 16 hex chars of SHA256.
fn sanitize_component(s: &str, max_len: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if cleaned.len() > max_len || cleaned.is_empty() {
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
    } else {
        cleaned
    }
}

/// Dual limits: if the file count exceeds max_files_per_thread or the directory's total bytes exceed max_thread_bytes → delete the oldest by mtime.
fn enforce_limits(dir: &std::path::Path, cfg: &CcrCfg) {
    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let md = e.metadata().ok()?;
                if !md.is_file() {
                    return None;
                }
                let mtime = md.modified().ok()?;
                Some((e.path(), mtime, md.len()))
            })
            .collect(),
        Err(_) => return,
    };
    // ascending by mtime (oldest first)
    entries.sort_by_key(|(_, mtime, _)| *mtime);
    let mut total: u64 = entries.iter().map(|(_, _, sz)| *sz).sum();
    let mut count = entries.len();
    for (path, _, sz) in &entries {
        let over_count = count > cfg.max_files_per_thread;
        let over_bytes = total > cfg.max_thread_bytes;
        if !over_count && !over_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            count -= 1;
            total = total.saturating_sub(*sz);
        }
    }
}
```

Add `pub mod ccr;` in `lib.rs`.

- [ ] **Step 5: Run the tests and pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test ccr_test`
Expected: PASS (5 tests). If flakiness appears due to concurrent HOME mutation, add serial per the Step 1 note.

- [ ] **Step 6: clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/ccr.rs zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/ccr_test.rs
git commit -m "feat(llm-compress-v2): Task10 ccr.rs 落盘+Text占位+sanitize+双限(核心总则:有损必可取回或返回原文)"
```
