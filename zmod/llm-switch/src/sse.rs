//! Shared SSE egress engine (Task 08).
//!
//! `run_egress` completes the HTTP POST + status-code check synchronously; non-2xx returns `Err` directly (never spawns);
//! only 2xx spawns the read task, feeds each `data:` line to the state machine, pushes `ResponseEvent`s into the channel,
//! and returns a `codex_api::ResponseStream` (§4.7).
//!
//! UTF-8 strategy: raw bytes accumulate into a `Vec<u8>`, and the frame bytes are UTF-8 decoded only after a complete
//! SSE frame boundary (the byte sequence `\n\n`) is found. `\n` (0x0A) never appears as a continuation byte of a
//! multi-byte character in UTF-8, so searching for `\n\n` at the byte level is safe; a complete frame guarantees
//! UTF-8 alignment and won't truncate in the middle of a multi-byte character (e.g. a CJK character), fundamentally
//! eliminating the replacement-character problem that `from_utf8_lossy` would produce.

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
    // ── Synchronous phase (§4.7): send request + status-code check ──────────────────────────────
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
        // Map third-party 401/403 to a plain Api error, avoiding triggering OpenAI recovery (§4.7)
        let text = resp.text().await.unwrap_or_default();
        return Err(ApiError::Api { status, message: text });
    }

    // ── Asynchronous phase: only spawn on 2xx ─────────────────────────────────────────
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

            // Split on SSE event boundaries (byte sequence \n\n), decoding UTF-8 only at complete frame boundaries
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
                        None => continue, // ignore event:/id:/comment lines
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
