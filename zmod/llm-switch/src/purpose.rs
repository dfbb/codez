//! 用途(Purpose)枚举与从 codex SessionSource 的映射(spec §4)。

use codex_protocol::protocol::{InternalSessionSource, SessionSource, SubAgentSource};

/// 内部用途(spec §4)。第一期三个:compact / review / memory。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    Compact,
    Review,
    Memory,
}

impl Purpose {
    /// 配置表 key,必须与 `[llm-switch.purpose]` 的键一致(spec §3)。
    pub fn as_key(self) -> &'static str {
        match self {
            Purpose::Compact => "compact",
            Purpose::Review => "review",
            Purpose::Memory => "memory",
        }
    }
}

/// 从 codex 的 SessionSource 解析用途;非内部子任务返回 None(spec §4 映射表)。
pub fn purpose_from_source(source: &SessionSource) -> Option<Purpose> {
    match source {
        SessionSource::SubAgent(SubAgentSource::Review) => Some(Purpose::Review),
        SessionSource::SubAgent(SubAgentSource::Compact) => Some(Purpose::Compact),
        SessionSource::SubAgent(SubAgentSource::MemoryConsolidation) => Some(Purpose::Memory),
        SessionSource::Internal(InternalSessionSource::MemoryConsolidation) => Some(Purpose::Memory),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::{SessionSource, SubAgentSource, InternalSessionSource};

    #[test]
    fn maps_subagent_variants() {
        assert_eq!(purpose_from_source(&SessionSource::SubAgent(SubAgentSource::Review)), Some(Purpose::Review));
        assert_eq!(purpose_from_source(&SessionSource::SubAgent(SubAgentSource::Compact)), Some(Purpose::Compact));
        assert_eq!(purpose_from_source(&SessionSource::SubAgent(SubAgentSource::MemoryConsolidation)), Some(Purpose::Memory));
    }

    #[test]
    fn maps_internal_memory() {
        assert_eq!(
            purpose_from_source(&SessionSource::Internal(InternalSessionSource::MemoryConsolidation)),
            Some(Purpose::Memory)
        );
    }

    #[test]
    fn main_sources_are_none() {
        assert_eq!(purpose_from_source(&SessionSource::Cli), None);
        assert_eq!(purpose_from_source(&SessionSource::VSCode), None);
        assert_eq!(purpose_from_source(&SessionSource::Exec), None);
        assert_eq!(purpose_from_source(&SessionSource::Mcp), None);
        assert_eq!(purpose_from_source(&SessionSource::Unknown), None);
        assert_eq!(purpose_from_source(&SessionSource::Custom("x".into())), None);
        assert_eq!(purpose_from_source(&SessionSource::SubAgent(SubAgentSource::Other("y".into()))), None);
    }

    #[test]
    fn as_key_matches_config_keys() {
        assert_eq!(Purpose::Compact.as_key(), "compact");
        assert_eq!(Purpose::Review.as_key(), "review");
        assert_eq!(Purpose::Memory.as_key(), "memory");
    }
}
