//! CCR: Lossy-compressed persisted fragment original text + Text path placeholder write path, model retrieves via shell/read (spec §4.7/E).
//! Core principle: When enabled, attach produces only "path placeholder included" or "original text returned", never "lossy but no path".

use crate::command::CommandHint;
use crate::config::CcrCfg;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// Context for a single request (Task 11 orchestration construction). Contains mutable CCR registry.
pub struct RequestCtx<'a> {
    pub queryid: &'a str,
    pub cmd_index: HashMap<String, CommandHint>,
    pub ccr: RefCell<CcrRegistry>,
}

/// Record (call_id, fragment_hash) → persisted file path, avoiding duplicate persistence of same fragment.
#[derive(Default)]
pub struct CcrRegistry {
    written: HashMap<(String, String), PathBuf>,
}

impl CcrRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

/// CCR root directory ~/.codex/llm-compress/ccr. None if HOME is not set.
pub fn ccr_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex").join("llm-compress").join("ccr"))
}

/// Persist fragment original text + append Text retrieval placeholder. Only called for lossy=true items.
/// See spec §4.7 core principle: When enabled, either "path placeholder included" or "original text returned".
pub fn attach(compressed: String, original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> String {
    if !cfg.enabled {
        return compressed; // disabled: preserve compressor's own placeholder, do not add path
    }
    // Single file exceeds limit → abandon compression, return original (ensure "lossy must be retrievable")
    if original.len() > cfg.max_file_bytes {
        return original.to_string();
    }
    match try_persist(original, ctx, call_id, cfg) {
        Some(path) => {
            let attached = format!("{compressed} [llm-compress: original {}]", path.display());
            // Secondary size gate: if path placeholder included exceeds original, abandon compression and return original (core principle: never leave "lossy without path")
            if attached.len() <= original.len() {
                attached
            } else {
                original.to_string()
            }
        }
        None => original.to_string(), // Persistence failed → return original (do not leave unretrievable lossy artifact)
    }
}

/// Persist: sanitize path, apply dual limits and cleanup, write file. Return path on success; None on any failure.
fn try_persist(original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> Option<PathBuf> {
    let frag_hash = short_hash(original);
    let key = (call_id.to_string(), frag_hash.clone());
    // Same fragment already persisted → reuse
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

/// First 12 hex digits of SHA256.
fn short_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Path component sanitize: non-[A-Za-z0-9_-] → '_'; exceeds max_len bytes → take first 16 hex of SHA256.
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

/// Dual limits: if file count exceeds max_files_per_thread or directory total bytes exceeds max_thread_bytes → delete oldest by mtime.
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
    // Sort by mtime ascending (oldest first)
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
