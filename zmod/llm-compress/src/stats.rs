//! 压缩统计 CSV 日志(spec §7)。
//!
//! 仅当一次请求整体有效压缩(saved_bytes>0)时,Task 08 的 transform 会调用
//! `log_compression` 写一行。本模块只负责"写一行",是否触发由调用方判断。
//!
//! 格式:CSV 四列,无表头,无引号:`时间戳,queryid,压缩前字节,压缩后字节`。
//! 时间戳为 RFC3339 UTC,秒精度,形如 `2026-06-20T08:15:30Z`。
//!
//! fail-open:写日志失败(目录建不了 / 权限 / 磁盘满)只记一条 `tracing::warn!`,
//! 绝不 panic、绝不返回 Err 阻断上层压缩流程。

use chrono::SecondsFormat;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 默认日志路径:`~/.codex/log/llm-compress.log`(用 HOME 环境变量解析)。
fn default_log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".codex")
            .join("log")
            .join("llm-compress.log"),
    )
}

/// 写一行压缩统计到 ~/.codex/log/llm-compress.log。失败仅 warn,不 panic。
///
/// 内部:解析默认路径、取当前 UTC 时间格式化为 RFC3339(秒精度,`...Z`),
/// 委托 `log_compression_to`;后者的 `Err` 在此被转成 `tracing::warn!` 吞掉。
pub fn log_compression(queryid: &str, before: usize, after: usize) {
    let path = match default_log_path() {
        Some(p) => p,
        None => {
            tracing::warn!("llm-compress: HOME unset, skip stats log");
            return;
        }
    };
    // RFC3339,UTC,秒精度,带 Z(use_z=true)
    let ts = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    if let Err(e) = log_compression_to(&path, &ts, queryid, before, after) {
        tracing::warn!("llm-compress: failed to write stats log {path:?}: {e}");
    }
}

/// 测试可注入路径与时间戳的内部版本。
///
/// 纯函数式:给定 path + 时间戳字符串。必要时 `create_dir_all` 父目录,
/// 以 append+create 模式打开,写 `format!("{ts},{qid},{before},{after}\n")`。
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
    // 清洗 queryid:剔除会破坏 CSV 行结构的字符(逗号 / CR / LF),保证恒为四列。
    // 当前 queryid = thread_id(UUID)本不含这些字符;此处加固公开 API,防未来其它调用方传入任意串。
    let safe_qid: String = queryid
        .chars()
        .filter(|&c| c != ',' && c != '\r' && c != '\n')
        .collect();
    let line = format!("{timestamp_rfc3339},{safe_qid},{before},{after}\n");
    file.write_all(line.as_bytes())?;
    Ok(())
}
