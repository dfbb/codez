//! 遍历 fixtures/inherited/manifest.toml,对每个继承样本跑硬不变量(spec §8.3)。
//! 不做逐字节相等;参考输出仅用于"体积不劣"对比。

use codez_llm_compress::compress::{
    diff::DiffCompressor, json::JsonCompressor, log::LogCompressor, search::SearchCompressor,
    tabular::TabularCompressor,
};
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inherited")
}

#[derive(serde::Deserialize)]
struct Manifest {
    fixture: Vec<Fixture>,
}
#[derive(serde::Deserialize)]
struct Fixture {
    file: String,
    compressor: String,
    #[serde(default)]
    ref_output: String,
}

fn run_compressor(name: &str, text: &str, cfg: &Config) -> Option<(String, bool)> {
    let budget = Budget { cfg, cmd: None };
    let c: Box<dyn Compressor> = match name {
        "json" => Box::new(JsonCompressor),
        "search" => Box::new(SearchCompressor),
        "tabular" => Box::new(TabularCompressor),
        "log" => Box::new(LogCompressor),
        "diff" => Box::new(DiffCompressor),
        _ => return None,
    };
    if !c.detect(text, &budget) {
        return None;
    }
    match c.compress(text, &budget) {
        CompressOutcome::Compressed { text, lossy, .. } => Some((text, lossy)),
        CompressOutcome::Unchanged => None,
    }
}

#[test]
fn parity_invariants_hold_for_all_fixtures() {
    let dir = fixtures_dir();
    let manifest_path = dir.join("manifest.toml");
    if !manifest_path.exists() {
        eprintln!("manifest.toml 不存在,跳过 parity(fixture 未就位)");
        return;
    }
    let manifest: Manifest =
        toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();

    let mut cfg = Config::disabled();
    cfg.enabled = true;
    // 给足阈值让压缩器认领(parity 关注算法输出而非让位)
    cfg.truncate.max_bytes = 1_000_000;

    for fx in &manifest.fixture {
        // preprocess 无直接对应 Compressor,跳过(preprocess 在 transform 流水线中测)
        if fx.compressor == "preprocess" {
            continue;
        }
        let input = std::fs::read_to_string(dir.join(&fx.file))
            .unwrap_or_else(|_| panic!("读不到 fixture {}", fx.file));
        let Some((out, lossy)) = run_compressor(&fx.compressor, &input, &cfg) else {
            continue; // 未认领/未压缩,跳过(允许)
        };

        // 硬不变量 1:压后 ≤ 压前
        assert!(out.len() <= input.len(), "[{}] 压后体积应 ≤ 压前", fx.file);
        // 硬不变量 2:UTF-8 合法(out 是 String,天然合法)
        // 硬不变量 3:JSON 压缩器产物可 parse
        if fx.compressor == "json" || fx.compressor == "tabular" {
            serde_json::from_str::<serde_json::Value>(&out)
                .unwrap_or_else(|_| panic!("[{}] JSON 产物必须可 parse", fx.file));
        }
        // 对比 4:体积不劣于参考(若有 ref_output)。
        // 仅当我方产物本身有损时才与参考比比例——headroom/rtk 的参考输出多为有损
        // (如 smart_crusher 抽样),而 v2 的 JSON/Tabular 是无损口径(spec §4.0/round-3
        // "JSON 不做有损"),无损产物天然压不过有损参考,比 1.5x 无意义。无损产物的
        // 正确性已由硬不变量 1(压后≤压前)保证,此处不再与有损参考比体积。
        if lossy && !fx.ref_output.is_empty() {
            if let Ok(reference) = std::fs::read_to_string(dir.join(&fx.ref_output)) {
                assert!(
                    out.len() as f64 <= reference.len() as f64 * 1.5,
                    "[{}] 我方产物 {} 不应远超参考 {} 的 1.5x",
                    fx.file,
                    out.len(),
                    reference.len()
                );
            }
        }
    }
}
