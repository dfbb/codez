# Task 2: purpose.rs — Purpose 枚举与 purpose_from_source

> 隶属 [llm-router 实现计划](README.md)。先读 README 的 Global Constraints。

**Files:**
- Create: `zmod/llm-switch/src/purpose.rs`
- Modify: `zmod/llm-switch/src/lib.rs`(注册 `mod purpose` + `pub use`)
- Test: 单元测试写在 `purpose.rs` 内 `#[cfg(test)]`(无外部依赖,内联即可)

**Interfaces:**
- Consumes: Task 1 的 `Config.purpose`(本 task 不直接用,只定义枚举与 source 映射)。
- Produces:
  - `pub enum Purpose { Compact, Review, Memory }`(derive `Debug, Clone, Copy, PartialEq, Eq, Hash`)。
  - `pub fn purpose_from_source(source: &codex_protocol::protocol::SessionSource) -> Option<Purpose>`。
  - `impl Purpose { pub fn as_key(self) -> &'static str }` 返回 `"compact"|"review"|"memory"`(供 Task 4 查 `Config.purpose` 表)。

---

## 背景(给零上下文工程师)

spec §4:把 codex 的 `SessionSource` 映射成内部 `Purpose`。`SessionSource` 定义在 `codex_protocol::protocol`(crate 已依赖 `codex-protocol`),相关变体(已核对 `protocol/src/protocol.rs:2565`):

```rust
pub enum SessionSource {
    Cli, VSCode, Exec, Mcp,
    Custom(String),
    Internal(InternalSessionSource),   // InternalSessionSource::MemoryConsolidation
    SubAgent(SubAgentSource),          // Review / Compact / ThreadSpawn{..} / MemoryConsolidation / Other(String)
    Unknown,
}
```

映射规则(spec §4 注释,逐条):

| SessionSource | Purpose |
|---|---|
| `SubAgent(Review)` | `Review` |
| `SubAgent(Compact)` | `Compact` |
| `SubAgent(MemoryConsolidation)` | `Memory` |
| `Internal(MemoryConsolidation)` | `Memory` |
| 其余(Cli/VSCode/Exec/Mcp/Custom/SubAgent(ThreadSpawn)/SubAgent(Other)/Unknown) | `None` |

`as_key` 必须与 spec §3 配置表的 key 完全一致(`compact`/`review`/`memory`),Task 4 用它做 `cfg.purpose.get(purpose.as_key())`。

---

- [ ] **Step 1: 写失败测试 — purpose.rs 内联单测**

创建 `zmod/llm-switch/src/purpose.rs`,先只写测试与待实现的占位签名:

```rust
//! 用途(Purpose)枚举与从 codex SessionSource 的映射(spec §4)。

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
```

- [ ] **Step 2: 在 lib.rs 注册模块,运行测试确认失败**

编辑 `zmod/llm-switch/src/lib.rs` 顶部模块声明区(当前 1-6 行 `mod config; ... mod sse;`)加一行:

```rust
mod purpose;
```

并在 `pub use` 区(当前 130-135 行附近)加导出:

```rust
pub use purpose::{purpose_from_source, Purpose};
```

运行:

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -20
```

Expected: 编译失败,`cannot find type 'Purpose'` / `cannot find function 'purpose_from_source'`(尚未实现)。

- [ ] **Step 3: 实现 Purpose 与 purpose_from_source**

在 `zmod/llm-switch/src/purpose.rs` 顶部(`#[cfg(test)]` 之前)写实现:

```rust
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
```

- [ ] **Step 4: 运行测试,确认通过**

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -20
```

Expected: 新增 4 个 purpose 测试 PASS,其余测试不受影响。

- [ ] **Step 5: clippy**

```bash
cd codex-rs && cargo clippy -p codez-llm-switch --all-targets 2>&1 | tail -15
```

Expected: 无 error;无本 task 新增 warning。

- [ ] **Step 6: 提交**

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git add zmod/llm-switch/src/purpose.rs zmod/llm-switch/src/lib.rs
git commit -m "feat(llm-switch): 新增 Purpose 枚举与 purpose_from_source(SessionSource 映射)"
```
