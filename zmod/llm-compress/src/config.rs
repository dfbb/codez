//! 读取 ~/.codex/config-zmod.toml 的 [llm_compress] 段。
//! fail-safe:文件/节缺失或解析失败 → enabled=false + 默认阈值。
//! `load()` 进程内读一次缓存(spec §5);`load_from()` 不缓存,供测试注入路径。

use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub min_total_bytes: usize,
    pub per_item_min_bytes: usize,
    pub truncate: TruncateCfg,
    pub json: JsonCfg,
    pub diff: DiffCfg,
    pub log: LogCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TruncateCfg {
    pub head_lines: usize,
    pub tail_lines: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JsonCfg {
    pub max_array_items: usize,
    pub max_depth: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiffCfg {
    pub context_lines: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LogCfg {
    pub dedup_repeats: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            min_total_bytes: 4096,
            per_item_min_bytes: 1024,
            truncate: TruncateCfg::default(),
            json: JsonCfg::default(),
            diff: DiffCfg::default(),
            log: LogCfg::default(),
        }
    }
}

impl Default for TruncateCfg {
    fn default() -> Self {
        Self { head_lines: 50, tail_lines: 50, max_bytes: 16384 }
    }
}

impl Default for JsonCfg {
    fn default() -> Self {
        Self { max_array_items: 20, max_depth: 6 }
    }
}

impl Default for DiffCfg {
    fn default() -> Self {
        Self { context_lines: 3 }
    }
}

impl Default for LogCfg {
    fn default() -> Self {
        Self { dedup_repeats: true }
    }
}

impl Config {
    pub fn disabled() -> Self {
        Self::default()
    }
}

/// 顶层文件结构:只关心 [llm_compress] 节。
#[derive(Debug, Deserialize)]
struct RootFile {
    #[serde(default)]
    llm_compress: Option<Config>,
}

fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex").join("config-zmod.toml"))
}

/// 从指定路径读取(便于测试注入)。
pub fn load_from(path: &std::path::Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Config::disabled(),
    };
    match toml::from_str::<RootFile>(&text) {
        Ok(root) => root.llm_compress.unwrap_or_else(Config::disabled),
        Err(e) => {
            // 仅记错误位置(行列),不回显 message——它可能携带用户配置内容片段。
            match e.span() {
                Some(span) => tracing::warn!(
                    "llm-compress: config parse failed at bytes {}..{}, disabling",
                    span.start,
                    span.end
                ),
                None => tracing::warn!("llm-compress: config parse failed, disabling"),
            }
            Config::disabled()
        }
    }
}

/// 从默认路径 ~/.codex/config-zmod.toml 读取。
///
/// 进程内读一次缓存(spec §5):首次解析后存入 `OnceLock`,后续请求复用,
/// 不再每次 transform 都读盘 + parse。测试用 `load_from` 绕开缓存。
pub fn load() -> Config {
    static CACHE: OnceLock<Config> = OnceLock::new();
    CACHE
        .get_or_init(|| match config_path() {
            Some(p) => load_from(&p),
            None => Config::disabled(),
        })
        .clone()
}
