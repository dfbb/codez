//! Shared SSE egress engine (Task 08).
//!
//! `run_egress` completes HTTP POST + status code verification synchronously, returns `Err`
//! immediately if not 2xx (never spawns); for 2xx, spawns a read task, feeds each `data:` line
//! to the state machine, pushes `ResponseEvent` into the channel, and returns
//! `codex_api::ResponseStream` (§4.7).
//!
//! UTF-8 strategy: raw bytes accumulate into `Vec<u8>`, UTF-8 decoding only happens after
//! finding a complete SSE frame boundary (byte sequence `\n\n`). `\n` (0x0A) never appears as
//! a trailing byte in a UTF-8 multibyte character, so searching for `\n\n` at the byte level
//! is safe; complete frames guarantee UTF-8 alignment and won't be truncated in the middle of
//! multibyte characters like Chinese, fundamentally eliminating the issue of
//! `from_utf8_lossy` producing replacement characters.

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
    // ── Sync phase (§4.7): send request + status code verification ──────────────────────────
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
        // Map third-party 401/403 to regular API errors to avoid triggering OpenAI recovery (§4.7)
        let text = resp.text().await.unwrap_or_default();
        return Err(ApiError::Api { status, message: text });
    }

    // ── Async phase: only spawn for 2xx ─────────────────────────────────────────
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

            // Split by SSE event boundary (byte sequence \n\n), decode UTF-8 only at complete frame boundaries
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
                        None => continue, // Skip event:/id:/comment lines
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

        // EOF or [DONE] → finish (synthesize assistant message + Completed, §4.5)
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
