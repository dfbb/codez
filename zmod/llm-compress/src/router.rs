//! 压缩器公共契约 + ContentRouter(命令感知重排 + first-match + fail-open)。

use crate::command::CommandHint;
use crate::config::Config;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// 产物形态。Json 恒配 lossy=false(spec §4.0 铁律)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Json,
}

/// Compressors read config / command hints from this.
pub struct Budget<'a> {
    pub cfg: &'a Config,
    pub cmd: Option<&'a CommandHint>,
}

/// 单个压缩器对一段文本的处理结果。
pub enum CompressOutcome {
    Compressed {
        text: String,
        saved_bytes: usize,
        lossy: bool,
        kind: ContentKind,
    },
    Unchanged,
}

/// 内容识别 + 压缩。detect 也吃 budget(拿 cmd 做命令感知认领)。
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str, budget: &Budget) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}

/// 固定优先级 + 命令感知重排;首个 detect 命中者压缩;detect/compress 均 catch_unwind。
pub struct ContentRouter {
    compressors: Vec<Box<dyn Compressor>>,
}

impl ContentRouter {
    pub fn new(compressors: Vec<Box<dyn Compressor>>) -> Self {
        Self { compressors }
    }

    /// 返回 Some((new, lossy, kind)) 仅当确有压缩(Compressed 且 saved_bytes>0);
    /// Unchanged / 无命中 / panic → None。命令提示命中时把对应压缩器提到候选最前。
    pub fn compress_text(&self, text: &str, budget: &Budget) -> Option<(String, bool, ContentKind)> {
        // 命令感知重排:命中的命令把对应 name 的压缩器排到最前(稳定,仅前移一个)。
        let preferred: Option<&'static str> = budget.cmd.and_then(|c| {
            if c.is_git_diff() {
                Some("diff")
            } else if c.is_grep() {
                Some("search")
            } else {
                None
            }
        });

        // 构造候选迭代顺序(索引列表):preferred 命中的先排,其余按原序。
        let mut order: Vec<usize> = (0..self.compressors.len()).collect();
        if let Some(name) = preferred {
            if let Some(pos) = self.compressors.iter().position(|c| c.name() == name) {
                let idx = order.remove(pos);
                order.insert(0, idx);
            }
        }

        let hit = order.into_iter().find(|&i| {
            let c = &self.compressors[i];
            catch_unwind(AssertUnwindSafe(|| c.detect(text, budget))).unwrap_or(false)
        })?;
        let c = &self.compressors[hit];

        let outcome = catch_unwind(AssertUnwindSafe(|| c.compress(text, budget)));
        match outcome {
            Ok(CompressOutcome::Compressed {
                text: new,
                saved_bytes,
                lossy,
                kind,
            }) if saved_bytes > 0 => Some((new, lossy, kind)),
            Ok(_) => None,
            Err(_) => {
                tracing::warn!("llm-compress: compressor '{}' panicked, passing through", c.name());
                None
            }
        }
    }
}
