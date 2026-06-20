//! 共享 SSE 出口引擎（Task 08）。
//!
//! `run_egress` 同步完成 HTTP POST + 状态码校验，非 2xx 直接 `Err`（绝不 spawn）；
//! 2xx 才 spawn 读取任务，逐 `data:` 行喂状态机，把 `ResponseEvent` 塞进 channel，
//! 返回 `codex_api::ResponseStream`（§4.7）。
//!
//! UTF-8 策略：使用 `String::from_utf8_lossy` 处理每个 chunk，在 `\n\n` SSE
//! 事件边界处切分。SSE 行格式要求 ASCII 可打印分隔符，帧内多字节 UTF-8 字符
//! 不会跨越 `\n\n` 边界，因此 lossy 解码在帧边界足够安全。

use futures::StreamExt;
use reqwest::header::HeaderMap;
use serde_json::Value;
use tokio::sync::mpsc;

use codex_api::{ApiError, ResponseEvent, ResponseStream};

use crate::connector::SseTranslator;

pub(crate) async fn run_egress(
    url: String,
    headers: HeaderMap,
    body: Value,
    http: reqwest::Client,
    mut translator: Box<dyn SseTranslator>,
) -> Result<ResponseStream, ApiError> {
    // ── 同步阶段（§4.7）：发请求 + 状态码校验 ──────────────────────────────
    let resp = http
        .post(&url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|e| ApiError::Stream(format!("request failed: {e}")))?;

    let upstream_request_id = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let status = resp.status();
    if !status.is_success() {
        // 第三方 401/403 映射成普通 Api 错误，避免触发 OpenAI recovery（§4.7）
        let text = resp.text().await.unwrap_or_default();
        return Err(ApiError::Api { status, message: text });
    }

    // ── 异步阶段：仅 2xx 才 spawn ─────────────────────────────────────────
    let (tx, rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
    let mut byte_stream = resp.bytes_stream();

    tokio::spawn(async move {
        let mut buf = String::new();
        let mut done = false;

        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx
                        .send(Err(ApiError::Stream(format!("stream error: {e}"))))
                        .await;
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // 按 SSE 事件边界（空行 \n\n）切分，逐 data: 处理
            while let Some(pos) = buf.find("\n\n") {
                let event_block = buf[..pos].to_string();
                buf.drain(..pos + 2);

                for line in event_block.lines() {
                    let line = line.trim_start();
                    let data = match line.strip_prefix("data:") {
                        Some(d) => d.trim(),
                        None => continue, // 忽略 event:/id:/注释行
                    };
                    if data == "[DONE]" {
                        done = true;
                        break;
                    }
                    if data.is_empty() {
                        continue;
                    }
                    let json: Value = match serde_json::from_str(data) {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = tx
                                .send(Err(ApiError::Stream(format!("bad SSE json: {e}"))))
                                .await;
                            return;
                        }
                    };
                    match translator.push(&json) {
                        Ok(events) => {
                            for ev in events {
                                if tx.send(Ok(ev)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Err(ce) => {
                            let _ = tx.send(Err(ce.into())).await;
                            return;
                        }
                    }
                }
                if done {
                    break;
                }
            }
            if done {
                break;
            }
        }

        // EOF 或 [DONE] → finish（合成 assistant message + Completed，§4.5）
        for ev in translator.finish() {
            if tx.send(Ok(ev)).await.is_err() {
                return;
            }
        }
    });

    Ok(ResponseStream {
        rx_event: rx,
        upstream_request_id,
    })
}
