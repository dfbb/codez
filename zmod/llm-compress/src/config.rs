//! 读取 ~/.codex/config-zmod.toml 的 [llm_compress] 段。
//! fail-safe:文件/节缺失或解析失败 → enabled=false + 默认阈值。

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub per_item_min_bytes: usize,
    pub truncate: TruncateCfg,
    pub json: JsonCfg,
    pub diff: DiffCfg,
    pub log: LogCfg,
    pub preprocess: PreprocessCfg,
    pub search: SearchCfg,
    pub tabular: TabularCfg,
    pub protect: ProtectCfg,
    pub ccr: CcrCfg,
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
    pub csv_schema: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiffCfg {
    pub context_lines: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LogCfg {
    pub keep_levels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PreprocessCfg {
    pub strip_progress: bool,
    pub collapse_blank: bool,
    pub truncate_line_bytes: usize,
    pub dedup_consecutive: bool,
    pub blob_min_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SearchCfg {
    pub max_per_file: usize,
    pub max_files: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TabularCfg {
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProtectCfg {
    pub error_max_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CcrCfg {
    pub enabled: bool,
    pub max_files_per_thread: usize,
    pub max_thread_bytes: u64,
    pub max_file_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            per_item_min_bytes: 1024,
            truncate: TruncateCfg::default(),
            json: JsonCfg::default(),
            diff: DiffCfg::default(),
            log: LogCfg::default(),
            preprocess: PreprocessCfg::default(),
            search: SearchCfg::default(),
            tabular: TabularCfg::default(),
            protect: ProtectCfg::default(),
            ccr: CcrCfg::default(),
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
        Self { csv_schema: true }
    }
}

impl Default for DiffCfg {
    fn default() -> Self {
        Self { context_lines: 3 }
    }
}

impl Default for LogCfg {
    fn default() -> Self {
        Self { keep_levels: vec!["error".to_string(), "warn".to_string()] }
    }
}

impl Config {
    pub fn disabled() -> Self {
        Self::default()
    }
}

impl Default for PreprocessCfg {
    fn default() -> Self {
        Self { strip_progress: true, collapse_blank: true, truncate_line_bytes: 2000, dedup_consecutive: true, blob_min_bytes: 256 }
    }
}
impl Default for SearchCfg {
    fn default() -> Self {
        Self { max_per_file: 5, max_files: 15 }
    }
}
impl Default for TabularCfg {
    fn default() -> Self {
        Self { enabled: true }
    }
}
impl Default for ProtectCfg {
    fn default() -> Self {
        Self { error_max_bytes: 8192 }
    }
}
impl Default for CcrCfg {
    fn default() -> Self {
        Self { enabled: true, max_files_per_thread: 200, max_thread_bytes: 67_108_864, max_file_bytes: 4_194_304 }
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
            tracing::warn!("llm-compress: config parse failed, disabling: {e}");
            Config::disabled()
        }
    }
}

/// 从默认路径 ~/.codex/config-zmod.toml 读取。
pub fn load() -> Config {
    match config_path() {
        Some(p) => load_from(&p),
        None => Config::disabled(),
    }
}
