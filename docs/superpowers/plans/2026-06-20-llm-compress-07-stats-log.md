# Task 07: stats.rs CSV Compression Stats Log

> Part of `2026-06-20-llm-compress-00-index.md`. Before starting, read the index's Global Constraints / real types / dev-build decisions.
>
> This task is **standalone**, depending only on the crate skeleton from Task 01 (the crate is already inside the codex-rs workspace, and `chrono` is already a dependency). It can run in parallel with 02–06.

**Goal:** Implement `stats.rs`—write one CSV line per effective compression to `~/.codex/log/llm-compress.log` (append, creating the directory if it doesn't exist). Provide two pinned public functions: the outward-facing `log_compression` (resolves the path + takes the current UTC time) and an injectable, purely functional `log_compression_to` (given a path + timestamp string). fail-open: on write failure, only `tracing::warn!`—never panic, never return an `Err` that would block the caller. Task 08's transform calls this module after deciding that the request is "effectively compressed overall (`saved_bytes>0`)"; **whether to trigger is the caller's decision**, and this module is only responsible for "writing one line".

**Spec coverage:** §7 (stats log).

**Files:**
- Create: `zmod/llm-compress/src/stats.rs`
- Create: `zmod/llm-compress/tests/stats_test.rs`
- Modify: `zmod/llm-compress/src/lib.rs` (add `pub mod stats;`)

**Interfaces:**
- Produces (Task 08 depends on these; signatures are pinned and must not change):
  ```rust
  /// Write one line of compression stats to ~/.codex/log/llm-compress.log. On failure, warn only, never panic.
  pub fn log_compression(queryid: &str, before: usize, after: usize);
  /// Internal version that lets tests inject the path and timestamp.
  pub fn log_compression_to(path: &std::path::Path, timestamp_rfc3339: &str, queryid: &str, before: usize, after: usize) -> std::io::Result<()>;
  ```
- Consumes: `chrono` (Task 01 already added `chrono = { version = "0.4", features = ["clock"] }`), `tempfile` (Task 01 already added it as a dev-dep).

**Format spec (strict):** CSV with four columns, **no header, no quotes**: `timestamp,queryid,bytes-before,bytes-after`. The timestamp is RFC3339 UTC (second precision, of the form `...Z`). Example line:
```
2026-06-20T08:15:30Z,019e3995-5cd9-75a2-b487-f7959835f69e,18432,5120
```

---

- [ ] **Step 1: Write the failing test**

Create `zmod/llm-compress/tests/stats_test.rs`:

```rust
//! stats.rs tests: use tempfile to inject the path and a fixed timestamp string,
//! verifying the exact line format, append semantics, automatic parent-dir creation,
//! and the four-column CSV with no quotes and no header.

use codez_llm_compress::stats::log_compression_to;
use std::fs;

#[test]
fn writes_exact_single_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    // Read back and assert exact equality (including the trailing newline)
    assert_eq!(content, "2026-06-20T08:15:30Z,abc,100,40\n");
}

#[test]
fn appends_second_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();
    log_compression_to(&path, "2026-06-20T09:00:00Z", "def", 200, 80).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "2026-06-20T08:15:30Z,abc,100,40");
    assert_eq!(lines[1], "2026-06-20T09:00:00Z,def,200,80");
}

#[test]
fn creates_missing_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    // A subdirectory under tempdir that doesn't yet exist
    let path = dir.path().join("nested").join("deeper").join("llm-compress.log");
    assert!(!path.parent().unwrap().exists());

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();

    assert!(path.exists());
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "2026-06-20T08:15:30Z,abc,100,40\n");
}

#[test]
fn line_format_is_four_columns_no_header_no_quotes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(
        &path,
        "2026-06-20T08:15:30Z",
        "019e3995-5cd9-75a2-b487-f7959835f69e",
        18432,
        5120,
    )
    .unwrap();

    let content = fs::read_to_string(&path).unwrap();
    // No quotes
    assert!(!content.contains('"'));
    // A single line (no header), comma-separated into exactly 4 columns
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1);
    let cols: Vec<&str> = lines[0].split(',').collect();
    assert_eq!(cols.len(), 4);
    assert_eq!(cols[0], "2026-06-20T08:15:30Z");
    assert_eq!(cols[1], "019e3995-5cd9-75a2-b487-f7959835f69e");
    assert_eq!(cols[2], "18432");
    assert_eq!(cols[3], "5120");
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test stats_test
```
Expected: compile error `unresolved import codez_llm_compress::stats` (Steps 3/4 are not yet implemented).

- [ ] **Step 3: Write the full stats.rs implementation**

Create `zmod/llm-compress/src/stats.rs`:

```rust
//! Compression stats CSV log (spec §7).
//!
//! Only when a request is effectively compressed overall (saved_bytes>0) does
//! Task 08's transform call `log_compression` to write one line. This module is
//! only responsible for "writing one line"; whether to trigger is the caller's decision.
//!
//! Format: CSV with four columns, no header, no quotes: `timestamp,queryid,bytes-before,bytes-after`.
//! The timestamp is RFC3339 UTC, second precision, of the form `2026-06-20T08:15:30Z`.
//!
//! fail-open: a logging failure (can't create the dir / permissions / disk full) only records
//! one `tracing::warn!`—never panics, never returns an `Err` that blocks the upstream compression flow.

use chrono::SecondsFormat;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Default log path: `~/.codex/log/llm-compress.log` (resolved via the HOME environment variable).
fn default_log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".codex")
            .join("log")
            .join("llm-compress.log"),
    )
}

/// Write one line of compression stats to ~/.codex/log/llm-compress.log. On failure, warn only, never panic.
///
/// Internally: resolve the default path, take the current UTC time and format it as RFC3339
/// (second precision, `...Z`), and delegate to `log_compression_to`; the latter's `Err` is
/// swallowed here into a `tracing::warn!`.
pub fn log_compression(queryid: &str, before: usize, after: usize) {
    let path = match default_log_path() {
        Some(p) => p,
        None => {
            tracing::warn!("llm-compress: HOME unset, skip stats log");
            return;
        }
    };
    // RFC3339, UTC, second precision, with Z (use_z=true)
    let ts = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    if let Err(e) = log_compression_to(&path, &ts, queryid, before, after) {
        tracing::warn!("llm-compress: failed to write stats log {path:?}: {e}");
    }
}

/// Internal version that lets tests inject the path and timestamp.
///
/// Purely functional: given a path + timestamp string. When necessary, `create_dir_all` the
/// parent dir, open in append+create mode, and write `format!("{ts},{qid},{before},{after}\n")`.
pub fn log_compression_to(
    path: &Path,
    timestamp_rfc3339: &str,
    queryid: &str,
    before: usize,
    after: usize,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    let line = format!("{timestamp_rfc3339},{queryid},{before},{after}\n");
    file.write_all(line.as_bytes())?;
    Ok(())
}
```

> Implementation checklist:
> - `to_rfc3339_opts(SecondsFormat::Secs, true)` yields a UTC value of the form `2026-06-20T08:15:30+00:00`—with `use_z=true` it emits a `Z` suffix, i.e. `2026-06-20T08:15:30Z`, satisfying "second precision / `...Z`".
> - `OpenOptions::append(true).create(true)`: create the file if it doesn't exist, append if it does, and **never** `truncate`.
> - The degenerate case where `path.parent()` is `None` (a bare filename with no parent dir) simply skips dir creation; the actual default path always has a parent dir.
> - Any write failure is returned as `Err` to `log_compression`, which swallows it via warn—`log_compression_to` itself does not warn and does not panic.

- [ ] **Step 4: Register the module in lib.rs**

Modify `zmod/llm-compress/src/lib.rs`, adding one module declaration (at the same level as the existing `pub mod config;`):

```rust
pub mod stats;
```

> Add only this one line; touch nothing else.

- [ ] **Step 5: Run the test and watch it pass**

Run (in the `codex-rs/` directory):
```bash
cargo test -p codez-llm-compress --test stats_test
```
Expected: `test result: ok. 4 passed`.

- [ ] **Step 6: Commit (only zmod + this plan file)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-07-stats-log.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): CSV compression stats log"
```

> **Do not** `git add codex-rs/...`—the dev-build enabler (the dirty line in core/Cargo.toml) stays uncommitted until Task 09.
