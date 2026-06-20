//! 压缩器公共契约 + ContentRouter(固定优先级 + fail-open)。

use crate::config::Config;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// 压缩器从中取自己的配置切片(如 budget.cfg.truncate)。
pub struct Budget<'a> {
    pub cfg: &'a Config,
}

/// 单个压缩器对一段文本的处理结果。
pub enum CompressOutcome {
    Compressed { text: String, saved_bytes: usize },
    Unchanged,
}

/// 内容识别 + 压缩。实现者保证 detect 廉价、compress 不依赖外部可变状态。
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}

/// 固定优先级路由:首个 detect 命中者负责压缩;compress 包在 catch_unwind 内 fail-open。
pub struct ContentRouter {
    compressors: Vec<Box<dyn Compressor>>,
}

impl ContentRouter {
    pub fn new(compressors: Vec<Box<dyn Compressor>>) -> Self {
        Self { compressors }
    }

    /// 返回 Some(new) 仅当确有压缩(Compressed 且 saved_bytes>0);
    /// Unchanged / 无命中 / panic → None(调用方保留原文)。
    pub fn compress_text(&self, text: &str, budget: &Budget) -> Option<String> {
        let c = self.compressors.iter().find(|c| {
            // detect 也兜 panic:有问题的压缩器视作不认领。
            catch_unwind(AssertUnwindSafe(|| c.detect(text))).unwrap_or(false)
        })?;

        let outcome = catch_unwind(AssertUnwindSafe(|| c.compress(text, budget)));
        match outcome {
            Ok(CompressOutcome::Compressed { text: new, saved_bytes }) if saved_bytes > 0 => {
                Some(new)
            }
            Ok(_) => None,
            Err(_) => {
                tracing::warn!("llm-compress: compressor '{}' panicked, passing through", c.name());
                None
            }
        }
    }
}
