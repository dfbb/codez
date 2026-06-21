# llm-compress zmod 设计文档

**日期**：2026-06-20
**crate**：`codez-llm-compress`（`zmod/llm-compress/`）
**目标**：在 codex 的 LLM 请求发送边界，对已组装好的 `ResponsesApiRequest` 做进程内压缩，降低发往上游的 token 体积。借鉴 headroom 的内容路由（content_router）与 rtk 的分段过滤管线（TOML DSL 8 段）。

---

## 1. 定位与边界

- **是什么**：一个独立 Rust crate，提供单一入口 `transform(request, provider, queryid) -> ResponsesApiRequest`，在请求发往上游前对其内容做不可逆但保守的压缩。
- **不是什么**：不换上游（那是姊妹 zmod `llm-switch` 的职责）、不改响应流、不做可逆检索（CCR）、不做 token 计数（留待迭代）。
- **与 llm-switch 的关系**：两者挂在 codex 同一个集成点，但**职责正交**。llm-compress 是**独立 crate、前置拦截**：先压缩、后路由。压缩对**所有**请求路径生效（含原生 OpenAI responses 路径），不依赖 llm-switch 是否命中路由。

---

## 2. 集成点（单点侵入）

**文件**：`codex-rs/core/src/client.rs`
**函数**：`stream_responses_api()`（约 1270 行）
**位置**：`build_responses_request(...)` 之后、构造 `ApiResponsesClient` / 进入路由分支之前（当前约 1318-1330 行）。

```rust
let mut request = self.client.build_responses_request(...)?;
let store = request.store;
self.client.prepare_response_items_for_request(&mut request.input, store);

// ── llm-compress 前置拦截（独立 zmod，不依赖 switch）──
let queryid = &responses_metadata.thread_id;   // codex 现成可达，见 §3
let request = codez_llm_compress::transform(
    request,
    &client_setup.api_provider,
    queryid,
);

// ── 原有路由分支（llm-switch 或原生），拿到的是已压缩的 request ──
let stream_result = match codez_llm_switch::route(...) {
    None => {
        let client = ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
        client.stream_request(request, options).await
    }
    Some(rt) => codez_llm_switch::run(rt, request, ...).await,
};
```

**关键性质**：

- `transform()` 入参出参都是 codex 原生 `ResponsesApiRequest`，对下游（switch / 原生 stream_request / SSE 解析 / 错误处理）完全透明。
- 关闭时（config 无 `[llm_compress]` 或 `enabled=false`）`transform()` 原样返回 request，等价零改动路径。
- codex 侧侵入仅两处，封装在 `patches/llm-compress.patch`：
  1. `core/Cargo.toml` 增加依赖 `codez-llm-compress = { path = "../../zmod/llm-compress" }`
  2. `client.rs` 增加上述 queryid 取值 + `transform()` 调用两行。
- **不修改任何 codex 函数签名**：`queryid` 由作用域内现成的入参 `responses_metadata.thread_id` 取得。

---

## 3. queryid 来源

`queryid` = `responses_metadata.thread_id`（`CodexResponsesMetadata` 字段，`pub(crate)`，core crate 内可达）。

> **修正（与实现/计划索引一致）**：早期草稿写 `session_id`，已改为 `thread_id`。原因：`session_id` 会被子 agent 继承父级、无法定位具体 rollout 文件；`thread_id`（`ThreadId`，UUIDv7）才与 rollout 文件名中的 UUID 精确对应。

该值即 codex 的 thread UUID，与 rollout 文件名中的 UUID 一致：

```
~/.codex/sessions/2026/05/18/rollout-2026-05-18T13-35-50-019e3995-5cd9-75a2-b487-f7959835f69e.jsonl
                                                          └────────────── thread_id ──────────────┘
```

来源链：`CodexResponsesMetadata.thread_id`（`core/src/responses_metadata.rs`，由 session 的 `ThreadId.to_string()` 填入）→ 作为入参 `responses_metadata: &CodexResponsesMetadata` 传入 `stream_responses_api`，patch 取 `responses_metadata.thread_id.clone()`。

因此压缩日志的 queryid 可与具体 rollout 文件精确对应。

---

## 4. 内部两层管线

`transform(request, provider, queryid) -> ResponsesApiRequest` 内部：

```
ResponsesApiRequest
   │
   ▼
[Layer 0] 开关 & 预算门
   • 读 config [llm_compress].enabled —— 关 → 原样返回
   • 估算 request input 文本总体积；低于 min_total_bytes → 原样返回（小请求不折腾）
   ▼
[Layer 1] 遍历 request.input[] 各项
   对每个 InputItem（主要是 function_call_output / 大文本项）:
     • 取文本载荷，低于 per_item_min_bytes → 跳过（保守阈值）
     ▼
   [Layer 2] ContentRouter 内容识别（借鉴 headroom content_router）
     按固定优先级依次 detect，第一个命中者负责压缩:
       ① JsonCompressor      （serde_json 能 parse）
       ② DiffCompressor      （含 @@/diff --git/--- a/ 头）
       ③ LogCompressor       （多行 + 重复行/时间戳/栈跟踪特征）
       ④ TruncateCompressor  （detect 永真，兜底）
     ▼
   [Layer 3] Compressor 内部流水（借鉴 rtk TOML DSL 分段）
     strip_ansi → (压缩器专属逻辑) → head/tail 保留 → max_bytes 截断
     → 插入占位标记 "[llm-compress: 略 N 行/字节]"
   ▼
[出口] 若整体 saved_bytes > 0 → 写统计日志（§7）；返回压缩后 request
```

### 核心 trait

```rust
/// 内容识别 + 压缩
trait Compressor: Send + Sync {
    fn name(&self) -> &'static str;
    fn detect(&self, text: &str) -> bool;            // 是否认领这段内容
    fn compress(&self, text: &str, budget: &Budget) -> CompressResult;
}

enum CompressResult {
    Compressed { text: String, saved_bytes: usize },
    Unchanged,                  // 认领了但判断不值得压
}
```

**ContentRouter**：固定优先级 `Json → Diff → Log → Truncate`，依次 `detect`，第一个命中的执行 `compress`。Truncate 永远兜底，保证任何超阈文本都有处理者。

**fail-open**：任一 compressor 在 `compress()` 中 panic 或异常（`std::panic::catch_unwind` 兜住）→ 该项**原文透传**，绝不让压缩失败影响请求。

---

## 5. 配置

文件：`~/.codex/config-zmod.toml`，节 `[llm_compress]`（与 llm-switch 同文件，独立节）。节缺失 = `enabled=false`（fail-safe）。读取在 `src/config.rs`，进程内读一次缓存。

```toml
[llm_compress]
enabled = false                 # 缺省关闭
min_total_bytes = 4096          # 请求 input 文本总量小于此值整体跳过
per_item_min_bytes = 1024       # 单项小于此值不压（保守阈值）

[llm_compress.truncate]
head_lines = 50
tail_lines = 50
max_bytes  = 16384              # 单项压后上限

[llm_compress.json]
max_array_items = 20            # 数组超此长度 → 抽样保留首尾 + 计数
max_depth = 6                   # 超深嵌套 → 截断为 "…"

[llm_compress.diff]
context_lines = 3               # 每个 hunk 保留的上下文行

[llm_compress.log]
dedup_repeats = true            # 折叠连续重复行为 "（上一行 ×N）"
```

---

## 6. 四个压缩器策略

| 压缩器 | detect 依据 | compress 策略 |
|--------|------------|--------------|
| **Json** | `serde_json::from_str` 成功 | 长数组抽样（首尾保留 + `"…(N more)"`）；超深嵌套截为 `"…"`；其余结构保留。**输出必须仍是合法 JSON**——压后重新 parse 失败则丢弃压缩结果、回退原文。 |
| **Diff** | 含 `@@ ... @@` / `diff --git` / `--- a/` 行 | 每个 hunk 保留变更行 + `context_lines` 行上下文，丢多余上下文，文件头保留。 |
| **Log** | 多行 + 时间戳 / 连续重复行 / `at ...:line` 栈特征 | 连续重复行折叠计数；保留 head/tail，中段折叠为 `[llm-compress: 略 N 行]`。 |
| **Truncate** | 永真（兜底） | strip ANSI → 保留 `head_lines` + `tail_lines`，中间替换为 `[llm-compress: 略 N 行 / M 字节]`。 |

**占位标记**统一格式 `[llm-compress: …]`，让模型明确知道此处有省略（不可逆但显式）。

---

## 7. 压缩统计日志

**文件**：`~/.codex/log/llm-compress.log`（目录不存在则创建；append 模式）。
**触发**：一次请求**有效压缩**（整体 `saved_bytes > 0`）后追加一行。直通 / 未命中 / 关闭状态**不记录**。
**格式**：CSV，四列，无表头：

```
时间戳,queryid,压缩前字节,压缩后字节
```

示例：

```
2026-06-20T08:15:30Z,019e3995-5cd9-75a2-b487-f7959835f69e,18432,5120
```

| 列 | 来源 |
|----|------|
| 时间戳 | RFC3339 UTC（`chrono`） |
| queryid | `responses_metadata.thread_id`（rollout 文件名 UUID） |
| 压缩前字节 | transform 入口 input 项文本总字节 |
| 压缩后字节 | transform 出口 input 项文本总字节 |

**大小口径**：input 项文本字节总和（压缩器实际作用对象），非整 request 序列化字节。
**实现**：`src/stats.rs` 的 `log_compression(queryid, before, after)`，`OpenOptions::append`。
**fail-open**：写日志失败（磁盘满 / 权限）仅记一条 tracing warn，绝不影响请求。

---

## 8. 错误处理（全程 fail-open）

- `transform()` 签名为 `-> ResponsesApiRequest`（**不返回 Result**），从类型上杜绝"压缩失败阻断请求"。
- 单个 compressor 内部 panic → `catch_unwind` 兜住 → 该项原文透传。
- config 解析失败 → 视为 `enabled=false`，记 warn，走零改动路径。
- JSON 压缩后必须能重新 parse；否则丢弃压缩结果、回退原文（不产出坏 JSON）。
- 统计日志写失败 → warn，不影响请求。

---

## 9. 测试策略

独立 crate，纯单元测试，不依赖 codex 运行时。

- **每个 compressor**：`detect` 真值表 + `compress` 快照测试（`insta`，real fixtures：真实 git diff、真实 JSON 工具输出、真实日志）。
- **ContentRouter**：优先级命中测试（一段既像 log 又能 parse JSON 时谁赢）。
- **fail-open**：注入会 panic 的假 compressor，断言原文透传。
- **阈值边界**：低于 `per_item_min_bytes` 不动；`enabled=false` 全直通且 `request` 逐字节不变。
- **不可逆但安全**：断言压后体积 ≤ 压前（不会越压越胖），占位标记存在。
- **统计日志**：有效压缩写一行 CSV、四列、格式正确；无压缩不写；写失败不 panic。

---

## 10. 可观测性

- `transform()` 内用 `tracing` 记 `saved_bytes` 汇总（debug 级），不污染正常输出。
- 占位标记本身是给模型和人看的可见信号。
- CSV 统计日志供离线分析压缩效果。
- v1 不做持久化指标 / 不做 token 计数（YAGNI）。

---

## 11. 模块文件布局

```
zmod/llm-compress/
  Cargo.toml                  # name = "codez-llm-compress"
  src/
    lib.rs                    # transform() 入口 + enabled()
    config.rs                 # 读 [llm_compress]
    stats.rs                  # 压缩统计日志 log_compression()
    router.rs                 # ContentRouter + Compressor trait + Budget
    compress/
      mod.rs
      truncate.rs
      json.rs
      diff.rs
      log.rs
  tests/
    fixtures/                 # 真实 diff/json/log 样本
    snapshots/                # insta 快照

patches/llm-compress.patch    # core/Cargo.toml + client.rs 两处改动
```

---

## 12. 关键决策记录

| 决策 | 选择 | 理由 |
|------|------|------|
| Connector/HTTP 处理 | 不自建，由后续路由（switch/原生）承担 | llm-compress 只变换 request，传输由下游负责，风险最低 |
| 与 llm-switch 关系 | 独立 crate，集成点前置拦截 | 压缩对所有路径生效，含原生 OpenAI；不绑定 switch 命中 |
| v1 压缩范围 | 内容路由 + 4 压缩器（Json/Diff/Log/Truncate） | 覆盖工具输出主要形态 |
| 可逆性 | 不可逆 + 保守阈值 + fail-open | 简单可靠；占位标记显式告知省略 |
| queryid | `responses_metadata.thread_id` | 现成可达、与 rollout 文件名 UUID 一致、子 agent 不串，不改 codex 签名 |
| 日志格式 | CSV 四列无表头 | 简洁，易解析 |
