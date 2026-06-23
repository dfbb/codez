//! Shared TOON encoding helper. Encodes a JSON `Value` to TOON and enforces
//! the mandatory round-trip self-check: a product is returned ONLY if decoding
//! it back yields a value byte-for-byte equal to the input. TOON is the model's
//! only view of the tool output, so a non-round-trippable encoding must be
//! discarded (fail-open) rather than written back.

use serde_json::Value;

/// Encode `value` to TOON. Returns `Some(toon)` iff encoding succeeds AND
/// `decode_default::<Value>(&toon) == *value`. Any encode error, decode error,
/// or inequality → `None` (caller falls back to the original text).
pub fn encode_checked(value: &Value) -> Option<String> {
    let toon = toon_format::encode_default(value).ok()?;
    let back: Value = toon_format::decode_default(&toon).ok()?;
    if back == *value {
        Some(toon)
    } else {
        None
    }
}
