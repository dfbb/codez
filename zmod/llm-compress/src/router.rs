//! Compressor common contract + ContentRouter (command-aware reordering + first-match + fail-open).

use crate::command::CommandHint;
use crate::config::Config;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Product form. Json always has lossy=false (spec §4.0 iron rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Text,
    Json,
    /// TOON product (Token-Oriented Object Notation). Not JSON; always
    /// lossless (`lossy=false`). The orchestrator treats it like `Json`:
    /// never appends a CCR pointer, and never re-parses it as JSON.
    Toon,
}

/// Compressors read config / command hints from this.
pub struct Budget<'a> {
    pub cfg: &'a Config,
    pub cmd: Option<&'a CommandHint>,
}

/// Single compressor's processing result for a text segment.
pub enum CompressOutcome {
    Compressed {
        text: String,
        saved_bytes: usize,
        lossy: bool,
        kind: ContentKind,
    },
    Unchanged,
}

/// Content detection + compression. detect also accepts budget (uses cmd for command-aware claiming).
pub trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str, budget: &Budget) -> bool;
    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome;
}

/// Fixed priority + command-aware reordering; first detect hit compresses; both detect/compress catch_unwind.
pub struct ContentRouter {
    compressors: Vec<Box<dyn Compressor>>,
}

impl ContentRouter {
    pub fn new(compressors: Vec<Box<dyn Compressor>>) -> Self {
        Self { compressors }
    }

    /// Returns Some((new, lossy, kind)) only if compression actually occurs (Compressed and saved_bytes>0);
    /// Unchanged / no hit / panic → None. When command hint matches, move the corresponding compressor to the front of candidates.
    pub fn compress_text(&self, text: &str, budget: &Budget) -> Option<(String, bool, ContentKind)> {
        // Command-aware reordering: when a command matches, move the compressor with the corresponding name to the front (stable, only moves one).
        let preferred: Option<&'static str> = budget.cmd.and_then(|c| {
            if c.is_git_diff() {
                Some("diff")
            } else if c.is_grep() {
                Some("search")
            } else {
                None
            }
        });

        // Construct candidate iteration order (index list): preferred hit comes first, rest in original order.
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
