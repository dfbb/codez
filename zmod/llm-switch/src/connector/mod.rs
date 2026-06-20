mod anthropic;
pub(crate) mod chat;
mod chat_req;

use async_trait::async_trait;
use thiserror::Error;

use crate::config::{AuthKind, Connector as ConnectorKind};

/// 连接器内部错误。
/// - `HardFail`：v1 不支持/结构无法表达（§4.0 等），映射成 `ApiError::InvalidRequest`。
/// - `Http`：已是 codex `ApiError`（建连/状态码/流错误）。
#[derive(Debug, Error)]
pub enum ConnError {
    #[error("llm-switch unsupported: {0}")]
    HardFail(String),
    #[error(transparent)]
    Http(#[from] codex_api::ApiError),
}

impl From<ConnError> for codex_api::ApiError {
    fn from(e: ConnError) -> Self {
        match e {
            ConnError::Http(api) => api,
            ConnError::HardFail(msg) => codex_api::ApiError::InvalidRequest { message: msg },
        }
    }
}

/// 出口上下文：由 Task 08 的 `run()` 从 `Route` + `resolve_key` 组装。
pub struct EgressCtx {
    pub base_url: String,
    pub model: String,
    pub auth: AuthKind,
    pub key: Option<String>,
    pub anthropic_version: Option<String>,
    pub path_override: Option<String>,
    pub default_max_tokens: Option<u32>,
    pub http: reqwest::Client,
}

#[async_trait]
pub trait Connector: Send + Sync {
    /// 同步完成 HTTP+状态码校验+SSE 建立后才 spawn（§4.7）；
    /// 返回与 `stream_request` 同型的流。
    async fn run(
        &self,
        req: codex_api::ResponsesApiRequest,
        ctx: &EgressCtx,
    ) -> Result<codex_api::ResponseStream, codex_api::ApiError>;
}

/// 工厂：按 `config::Connector` 枚举选择具体连接器实现。
pub fn make_connector(kind: ConnectorKind) -> Box<dyn Connector> {
    match kind {
        ConnectorKind::Chat => Box::new(chat::ChatConnector),
        ConnectorKind::Anthropic => Box::new(anthropic::AnthropicConnector),
    }
}
