//! 共享 SSE 出口引擎（Task 08）。
//!
//! `run_egress` 同步完成 HTTP POST + 状态码校验，非 2xx 直接 `Err`（绝不 spawn）；
//! 2xx 才 spawn 读取任务，逐 `data:` 行喂状态机，把 `ResponseEvent` 塞进 channel，
//! 返回 `codex_api::ResponseStream`（§4.7）。
//!
//! UTF-8 策略：原始字节累积进 `Vec<u8>`，仅在找到完整 SSE 帧边界（字节序列
//! `\n\n`）后才对帧字节做 UTF-8 解码。`\n`（0x0A）在 UTF-8 中绝不会作为多字节
//! 字符的后续字节，故在字节层面查找 `\n\n` 安全；完整帧保证 UTF-8 对齐，不会
//! 在汉字等多字节字符中间截断，从根本上消除 `from_utf8_lossy` 产生替换字符的
//! 问题。

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
        let mut buf: Vec<u8> = Vec::new();
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
            buf.extend_from_slice(&chunk);

            // 按 SSE 事件边界（字节序列 \n\n）切分，仅在完整帧边界解码 UTF-8
            while let Some(pos) = buf.windows(2).position(|w| w == b"\n\n") {
                let frame_bytes = buf[..pos].to_vec();
                buf.drain(..pos + 2);

                let event_block = match std::str::from_utf8(&frame_bytes) {
                    Ok(s) => s.to_string(),
                    Err(e) => {
                        let _ = tx
                            .send(Err(ApiError::Stream(format!("SSE frame UTF-8 error: {e}"))))
                            .await;
                        return;
                    }
                };

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
