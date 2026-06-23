//! JsonCompressor — encodes object/array JSON tool-output as TOON.
//! Product is kind=Toon, lossy=false. Claims only when the TOON form passes
//! the round-trip self-check, is strictly smaller than the input, and fits
//! within truncate.max_bytes; otherwise yields to the next compressor
//! (ultimately Truncate). detect and compress share `try_toon` so they agree.

use crate::compress::toon::encode_checked;
use crate::router::{Budget, CompressOutcome, Compressor, ContentKind};
use serde_json::Value;

pub struct JsonCompressor;

/// Run the full claim pipeline. Returns Some(toon) iff:
///   use_toon, parses to object/array, encodes + round-trips,
///   toon.len() < text.len(), and toon.len() <= truncate.max_bytes.
fn try_toon(text: &str, budget: &Budget) -> Option<String> {
    if !budget.cfg.json.use_toon {
        return None;
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(v @ Value::Object(_)) | Ok(v @ Value::Array(_)) => v,
        _ => return None,
    };
    let toon = encode_checked(&value)?;
    if toon.len() < text.len() && toon.len() <= budget.cfg.truncate.max_bytes {
        Some(toon)
    } else {
        None
    }
}

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    fn detect(&self, text: &str, budget: &Budget) -> bool {
        try_toon(text, budget).is_some()
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        match try_toon(text, budget) {
            Some(toon) => {
                let saved = text.len().saturating_sub(toon.len());
                if saved == 0 {
                    return CompressOutcome::Unchanged;
                }
                CompressOutcome::Compressed {
                    text: toon,
                    saved_bytes: saved,
                    lossy: false,
                    kind: ContentKind::Toon,
                }
            }
            None => CompressOutcome::Unchanged,
        }
    }
}
