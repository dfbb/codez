//! CCR:有损压缩落盘片段原文 + Text 占位写路径,模型用 shell/read 取回(spec §4.7/E)。
//! 核心总则:enabled 下 attach 只产"含路径占位"或"返回原文",绝无"有损但无路径"。

use crate::command::CommandHint;
use crate::config::CcrCfg;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// 一次请求的上下文(Task 11 编排构造)。含可变 CCR registry。
pub struct RequestCtx<'a> {
    pub queryid: &'a str,
    pub cmd_index: HashMap<String, CommandHint>,
    pub ccr: RefCell<CcrRegistry>,
}

/// 记 (call_id, fragment_hash) → 已落盘文件路径,避免同片段重复落盘。
#[derive(Default)]
pub struct CcrRegistry {
    written: HashMap<(String, String), PathBuf>,
}

impl CcrRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

/// CCR 根目录 ~/.codex/llm-compress/ccr。HOME 未设 → None。
pub fn ccr_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex").join("llm-compress").join("ccr"))
}

/// 落盘片段原文 + 追加 Text 取回占位。仅 lossy=true 项调用。
/// 见 spec §4.7 核心总则:enabled 下要么"含路径占位",要么"返回原文"。
pub fn attach(compressed: String, original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> String {
    if !cfg.enabled {
        return compressed; // disabled:保留压缩器自身占位,不加路径
    }
    // 单文件超限 → 放弃压缩,返回原文(保"有损必可取回")
    if original.len() > cfg.max_file_bytes {
        return original.to_string();
    }
    match try_persist(original, ctx, call_id, cfg) {
        Some(path) => {
            let attached = format!("{compressed} [llm-compress: 原文 {}]", path.display());
            // 二次体积闸门:含路径占位若超原文,放弃压缩返回原文(核心总则:绝不留"有损无路径")
            if attached.len() <= original.len() {
                attached
            } else {
                original.to_string()
            }
        }
        None => original.to_string(), // 落盘失败 → 返回原文(不留下不可取回有损产物)
    }
}

/// 落盘:sanitize 路径、双限清理、写文件。成功返回路径;任何失败返回 None。
fn try_persist(original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> Option<PathBuf> {
    let frag_hash = short_hash(original);
    let key = (call_id.to_string(), frag_hash.clone());
    // 同片段已落盘 → 复用
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

/// SHA256 前 12 hex。
fn short_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// 路径组件 sanitize:非 [A-Za-z0-9_-] → '_';超 max_len 字节 → 取 SHA256 前 16 hex。
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

/// 双限:文件数超 max_files_per_thread 或目录总字节超 max_thread_bytes → 按 mtime 删最旧。
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
    // 按 mtime 升序(最旧在前)
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
