//! JsonCompressor —— JSON 结构内压缩,绝不破坏 JSON。
//!
//! 只认领可被 serde_json 解析的文本;在 Value 结构内部递归抽样长数组、
//! 裁剪超深子树,占位一律用合法 JSON 值承载;序列化回紧凑 JSON 后再次
//! parse 校验,失败或无收益则回退原文。

use crate::config::JsonCfg;
use crate::router::{Budget, CompressOutcome, Compressor};
use serde_json::Value;

/// 无状态单元结构。
pub struct JsonCompressor;

impl Compressor for JsonCompressor {
    fn name(&self) -> &'static str {
        "json"
    }

    /// 能被 serde_json 解析为对象/数组才认领。
    ///
    /// 顶层标量(字符串/数字/布尔/null)虽是合法 JSON,但 JsonCompressor 对其
    /// 无结构可压(只会 Unchanged),若认领会挡住兜底链——例如一段几十 KB 的纯
    /// 文本恰好是合法 JSON 字符串字面量时,Truncate 永远轮不到。故让渡给下游。
    fn detect(&self, text: &str) -> bool {
        matches!(
            serde_json::from_str::<Value>(text),
            Ok(Value::Object(_)) | Ok(Value::Array(_))
        )
    }

    fn compress(&self, text: &str, budget: &Budget) -> CompressOutcome {
        // 1. parse(失败 → Unchanged;detect 已保证,这里是防御性回退)。
        let mut value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return CompressOutcome::Unchanged,
        };

        // 2. 结构内递归压缩。
        let cfg = &budget.cfg.json;
        compress_value(&mut value, 0, cfg);

        // 3. 序列化回紧凑 JSON。
        let new = match serde_json::to_string(&value) {
            Ok(s) => s,
            Err(_) => return CompressOutcome::Unchanged,
        };

        // 4. 校验:产物必须能被重新解析,否则丢弃、回退原文。
        if serde_json::from_str::<Value>(&new).is_err() {
            return CompressOutcome::Unchanged;
        }

        // 5. 仅在确有收益时返回 Compressed。
        let saved_bytes = text.len().saturating_sub(new.len());
        if saved_bytes > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// 原地递归压缩一个 Value。
///
/// - `depth`:当前节点所在深度(顶层为 0)。
/// - 超深(`depth > max_depth`)的容器(对象/数组)整体替换为 `"…"`。
/// - 长数组(`len > max_array_items`)抽样:留头 ceil、留尾 floor,中间插一个 `"…(N more)"`。
/// - 对象键全保留,逐值递归;数组逐元素递归。标量原样保留。
fn compress_value(v: &mut Value, depth: usize, cfg: &JsonCfg) {
    // 超深:仅裁容器,标量留着(替换标量为 "…" 无收益且可能更长)。
    if depth > cfg.max_depth && (v.is_object() || v.is_array()) {
        *v = Value::String("…".to_string());
        return;
    }

    match v {
        Value::Array(items) => {
            // 先抽样(如超长),再对保留下来的元素递归(深度 +1)。
            if items.len() > cfg.max_array_items {
                let len = items.len();
                let keep_head = cfg.max_array_items.div_ceil(2); // ceil
                let keep_tail = cfg.max_array_items / 2; // floor
                let omitted = len.saturating_sub(keep_head + keep_tail);

                // 取尾段(后 keep_tail 个),再取头段(前 keep_head 个),
                // 用 [头…] + 占位 + [尾…] 重组。
                let tail: Vec<Value> = items.split_off(len - keep_tail);
                items.truncate(keep_head);
                items.push(Value::String(format!("…({omitted} more)")));
                items.extend(tail);
            }

            for child in items.iter_mut() {
                // 占位字符串元素也会被这层递归扫到,但它是标量,不受影响。
                compress_value(child, depth + 1, cfg);
            }
        }
        Value::Object(map) => {
            for (_k, child) in map.iter_mut() {
                compress_value(child, depth + 1, cfg);
            }
        }
        // 标量:原样保留。
        _ => {}
    }
}
