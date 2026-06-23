# Task 3: namespace.rs — request_has_namespace_tools 预检

> 隶属 [llm-router 实现计划](README.md)。先读 README 的 Global Constraints。

**Files:**
- Create: `zmod/llm-switch/src/namespace.rs`
- Modify: `zmod/llm-switch/src/lib.rs`(注册 `mod namespace` + `pub use`)
- Test: 单元测试写在 `namespace.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Consumes:(无跨 task 依赖;直接用 codex-api 类型)
- Produces:
  - `pub fn request_has_namespace_tools(req: &codex_api::ResponsesApiRequest) -> bool`(供 Task 4 在 route() purpose 分支调用)。

---

## 背景(给零上下文工程师)

spec §4.1:purpose 命中后,若请求含 llm-switch v1 **不可表达**的工具,要在 route() 内放弃用途路由、回退 provider-id(fail-safe),而**不是**让连接器深处硬失败。判定函数必须**同时覆盖两个独立来源**(已核对类型):

`ResponsesApiRequest`(`codex-api/src/common.rs:183`)相关字段:
- `pub tools: Vec<serde_json::Value>` —— 工具定义。连接器 `map_tools` 只接受 `{"type":"function"}`,其余类型硬失败。**review 子 agent 首轮的 `mcp__...` namespace 工具定义在这里**。
- `pub input: Vec<ResponseItem>` —— 对话项。其中 `ResponseItem::FunctionCall { namespace: Option<String>, .. }`(`protocol/src/models.rs:954`),`namespace.is_some()` 时连接器硬失败(后续轮次)。

判定规则:
- `tools` 里**任一**元素的 `type` 字段不是 `"function"`(含缺失 `type`)→ true。
- `input` 里**任一** `ResponseItem::FunctionCall` 的 `namespace` 为 `Some` → true。
- 两者皆无 → false。

> 注:`tools` 是 `serde_json::Value`,用 `.get("type").and_then(|v| v.as_str())` 取类型;取不到(非对象 / 无 type)按「不是 function」处理 → true(保守:不可表达就降级)。

---

- [ ] **Step 1: 写失败测试**

创建 `zmod/llm-switch/src/namespace.rs`,先写测试与占位:

```rust
//! 请求是否含 llm-switch v1 不可表达的工具(spec §4.1 预检)。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_request;

    #[test]
    fn plain_function_tool_is_expressible() {
        let mut req = sample_request();
        req.tools = vec![serde_json::json!({"type": "function", "name": "shell"})];
        assert!(!request_has_namespace_tools(&req));
    }

    #[test]
    fn non_function_tool_definition_triggers() {
        let mut req = sample_request();
        req.tools = vec![serde_json::json!({"type": "web_search"})];
        assert!(request_has_namespace_tools(&req));
    }

    #[test]
    fn tool_without_type_triggers() {
        let mut req = sample_request();
        req.tools = vec![serde_json::json!({"name": "weird"})];
        assert!(request_has_namespace_tools(&req));
    }

    #[test]
    fn namespaced_function_call_in_input_triggers() {
        let mut req = sample_request();
        req.input = vec![codex_protocol::models::ResponseItem::FunctionCall {
            id: None,
            name: "send".into(),
            namespace: Some("mcp__gmail".into()),
            arguments: "{}".into(),
            call_id: "c1".into(),
            metadata: None,
        }];
        assert!(request_has_namespace_tools(&req));
    }

    #[test]
    fn plain_function_call_in_input_is_expressible() {
        let mut req = sample_request();
        req.input = vec![codex_protocol::models::ResponseItem::FunctionCall {
            id: None,
            name: "shell".into(),
            namespace: None,
            arguments: "{}".into(),
            call_id: "c1".into(),
            metadata: None,
        }];
        assert!(!request_has_namespace_tools(&req));
    }

    #[test]
    fn empty_request_is_expressible() {
        let req = sample_request();
        assert!(!request_has_namespace_tools(&req));
    }
}
```

> 测试用到的 `sample_request()` 已存在于 `lib.rs` 的 `pub mod testing`(返回 `tools: vec![]`、`input: vec![]` 的最小请求);`#[cfg(test)]` 单测里 `use crate::testing::sample_request;` 即可复用。

- [ ] **Step 2: 在 lib.rs 注册模块,运行测试确认失败**

编辑 `zmod/llm-switch/src/lib.rs` 模块声明区加:

```rust
mod namespace;
```

`pub use` 区加:

```rust
pub use namespace::request_has_namespace_tools;
```

运行:

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -20
```

Expected: 编译失败,`cannot find function 'request_has_namespace_tools'`。

- [ ] **Step 3: 实现 request_has_namespace_tools**

在 `zmod/llm-switch/src/namespace.rs` 顶部写实现:

```rust
use codex_protocol::models::ResponseItem;

/// 请求是否含 v1 连接器不可表达的工具(spec §4.1)。
/// 覆盖两个硬失败来源:tools 里的非 function 工具定义、input 里的 namespaced FunctionCall。
pub fn request_has_namespace_tools(req: &codex_api::ResponsesApiRequest) -> bool {
    // ① 工具定义:任一非 "function" 类型(含缺失 type)→ 不可表达
    let bad_tool_def = req.tools.iter().any(|t| {
        t.get("type").and_then(|v| v.as_str()) != Some("function")
    });
    if bad_tool_def {
        return true;
    }
    // ② 函数调用:任一带 namespace 的 FunctionCall → 不可表达
    req.input.iter().any(|item| {
        matches!(item, ResponseItem::FunctionCall { namespace: Some(_), .. })
    })
}
```

- [ ] **Step 4: 运行测试,确认通过**

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -20
```

Expected: 新增 6 个 namespace 测试 PASS。

- [ ] **Step 5: clippy**

```bash
cd codex-rs && cargo clippy -p codez-llm-switch --all-targets 2>&1 | tail -15
```

Expected: 无 error;无本 task 新增 warning。

- [ ] **Step 6: 提交**

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git add zmod/llm-switch/src/namespace.rs zmod/llm-switch/src/lib.rs
git commit -m "feat(llm-switch): 新增 request_has_namespace_tools 不可表达工具预检"
```
