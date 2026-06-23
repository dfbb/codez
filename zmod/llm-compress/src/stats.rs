//! Compression statistics CSV log (spec §7).
//!
//! The transform in Task 08 calls `log_compression` to write one line only when
//! a single request has valid compression overall (saved_bytes>0). This module only
//! handles "writing one line"; whether to trigger is determined by the caller.
//!
//! Format: CSV with four columns, no header, no quotes: `timestamp,queryid,bytes_before,bytes_after`.
//! Timestamp is RFC3339 UTC with second precision, e.g. `2026-06-20T08:15:30Z`.
//!
//! fail-open: logging failures (cannot create directory / permission denied / disk full)
//! are logged only as `tracing::warn!`, never panicking or returning Err to block
//! the upper compression pipeline.

use chrono::SecondsFormat;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Default log path: `~/.codex/log/llm-compress.log` (resolved using the HOME environment variable).
fn default_log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".codex")
            .join("log")
            .join("llm-compress.log"),
    )
}

/// Write one compression statistic line to ~/.codex/log/llm-compress.log. Failures only warn, never panic.
///
/// Internally: parse the default path, get the current UTC time and format it as RFC3339
/// (second precision, `...Z`), then delegate to `log_compression_to`; any Err from the latter
/// is converted to `tracing::warn!` and swallowed here.
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

/// Internal version that allows injection of path and timestamp for testing.
///
/// Pure functional: given path + timestamp string. Create parent directories
/// with `create_dir_all` if necessary, open in append+create mode,
/// write `format!("{ts},{qid},{before},{after}\n")`.
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
