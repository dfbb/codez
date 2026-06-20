# llm-compress 实现计划 — 总索引

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development(推荐)或 superpowers:executing-plans 逐任务执行。每个任务是一个独立 plan 文件,步骤用 `- [ ]` 复选框追踪。

**Goal:** 实现 codez 第二个 zmod `codez-llm-compress`,在 codex 的 LLM 请求发送边界(`core/src/client.rs` 的 `stream_responses_api`)对已组装好的 `ResponsesApiRequest` 做进程内、不可逆但保守的压缩,降低发往上游的 token 体积,并把每次有效压缩记入 CSV 统计日志。

**Architecture:** 单一入口 `transform(&mut request, &api_provider, queryid)`,在 `prepare_response_items_for_request` 之后、`record_started` 之前插入一次,**先压缩后路由**,对下游(原生 `stream_request` / 将来的 llm-switch / SSE 解析)完全透明。内部两层管线:Layer 0 开关&预算门 → Layer 1 遍历 `Vec<ResponseItem>` 取工具输出文本 → Layer 2 ContentRouter 按内容识别选压缩器 → Layer 3 压缩器内部流水(文本型 head/tail 截断+裸占位;结构型 JSON 内压缩+parse 校验)。对 codex-rs 的侵入用 `patches/llm-compress.patch` 表达,不直接改源码。

**Tech Stack:** Rust(edition 2021)、serde / serde_json、toml、chrono(日志时间戳)、tracing;dev-dep `insta`(快照)。反向 path 依赖 `codex-api` / `codex-protocol`。

**设计依据:** `docs/superpowers/specs/2026-06-20-llm-compress-design.md`(已定稿并经三维源码核对,commit `58bbbf4ff`)。本计划每个任务标注其覆盖的 spec 小节。

---

## Global Constraints

逐条照抄自 spec,每个任务的要求隐含包含本节:

- **crate 命名**:包名 `codez-llm-compress`,目录 `zmod/llm-compress/`,lib target `codez_llm_compress`(spec §11)。
- **不声明自己的 `[workspace]`**:否则被当 path 依赖编译时触发 nested-workspace 报错。
- **反向 path 依赖**:`codex-api = { path = "../../codex-rs/codex-api" }`、`codex-protocol = { path = "../../codex-rs/protocol" }`,**不**用 `workspace = true`。
- **生产接入不进 workspace members**:zmod 在 codex-rs root 树外;**生产** patch 由 `codex-rs/core/Cargo.toml` 加外部 path 依赖 `codez-llm-compress = { path = "../../zmod/llm-compress" }` 接入,不进 members(spec §11)。**开发期**另用软链 member(见下「开发期构建与测试」),两者并存、互不影响。
- **集成点单点侵入**:仅在 `core/src/client.rs` `stream_responses_api` 的 `prepare_response_items_for_request` 之后、`record_started` 之前插入两行(queryid 取值 + `transform` 调用);不改任何 codex 函数签名(spec §2)。
- **transform 签名钉死**:`pub fn transform(request: &mut ResponsesApiRequest, api_provider: &ApiProvider, queryid: &str)`,返回 `()`。第二参短借用、只读判别,**不得**克隆/持有/返回该引用;返回 `()` 而非 `Result`,从类型上杜绝压缩失败阻断请求(spec §1/§2/§8)。
- **queryid = `responses_metadata.thread_id`**:与 rollout 文件名 UUID 精确对应;**不**用 `session_id`(子 agent 继承父级,无法定位具体 rollout 文件)(spec §3)。
- **只处理两个 ResponseItem 变体**:`FunctionCallOutput` 与 `CustomToolCallOutput`(二者 `output: FunctionCallOutputPayload`);其余变体一律不动。MCP 输出经 `impl From<ResponseInputItem> for ResponseItem` 已转成 `FunctionCallOutput`,已落在范围内(spec §4)。
- **文本提取规则**:`FunctionCallOutputBody::Text(s)` → 压 `s`;`FunctionCallOutputBody::ContentItems(items)` → **逐项仅压** `InputText{text}` 的 `text`,`InputImage`/`EncryptedContent` 不读不改;**绝不** flatten ContentItems(spec §4)。
- **fail-open 贯穿**:任一压缩器 `compress()` panic → `catch_unwind` 兜住 → 该片段原文透传;config 解析失败 → 视 `enabled=false` + warn;JSON 压后 parse 失败 → 丢弃压缩、回退原文;日志写失败 → warn,不影响请求(spec §6/§8)。
- **不可逆 + 保守阈值**:只压超阈大项;占位标记显式告知省略——文本型裸标记 `[llm-compress: …]`,结构型(JSON)用合法 JSON 值承载(spec §6)。
- **JSON 不走文本级流程**:JsonCompressor 必须在 JSON 结构内压缩,占位以 JSON 值表达,产物必经 `serde_json` 重新 parse 校验,失败回退原文;**不得**对 JSON 做文本级 head/tail 截断或插裸标记(spec §4/§6)。
- **统计日志**:`~/.codex/log/llm-compress.log`,append;仅整体 `saved_bytes>0` 时写一行 CSV 四列无表头:`时间戳(RFC3339 UTC),queryid,压缩前字节,压缩后字节`;大小口径 = input 项文本字节总和(spec §7)。
- **配置 fail-safe**:`~/.codex/config-zmod.toml` 无 `[llm_compress]` 节或 `enabled=false` → 整体关闭、零改动路径(spec §5)。
- **Rust 风格**:非测试代码避免 `unwrap`/`expect`(catch_unwind 内的 panic 边界除外)。

---

## 实现层钉死的真实类型(避免按记忆猜,均经源码核实)

- 集成点:`core/src/client.rs:1309` `let mut request = self.client.build_responses_request(...)?;`;1318-1320 `let store = request.store; self.client.prepare_response_items_for_request(&mut request.input, store);`;1321 `inference_trace.start_attempt()`;1323 `record_started(&request)`;1324-1330 `ApiResponsesClient::new(transport, client_setup.api_provider, client_setup.api_auth).with_telemetry(...).stream_request(request, options)`。
- `ResponsesApiRequest.input: Vec<ResponseItem>`(`prepare_response_items_for_request(&mut [ResponseItem], bool)` 佐证)。
- `enum ResponseItem`(`protocol/src/models.rs`)16 变体:`Message, AgentMessage, Reasoning, LocalShellCall, FunctionCall, ToolSearchCall, FunctionCallOutput, CustomToolCall, CustomToolCallOutput, ToolSearchOutput, WebSearchCall, ImageGenerationCall, Compaction, CompactionTrigger, ContextCompaction, Other`——**不含** `McpToolCallOutput`(后者属 `ResponseInputItem`,models.rs:820)。
- `FunctionCallOutput { call_id: String, output: FunctionCallOutputPayload }`(models.rs:1010 附近);`CustomToolCallOutput { call_id: String, name: Option<String>, output: FunctionCallOutputPayload }`(models.rs:1042 附近)。
- `FunctionCallOutputPayload { body: FunctionCallOutputBody, success: Option<bool> }`(models.rs:1778);`enum FunctionCallOutputBody { Text(String), ContentItems(Vec<FunctionCallOutputContentItem>) }`(models.rs:1785);`enum FunctionCallOutputContentItem { InputText{text:String}, InputImage{image_url:String, detail:Option<ImageDetail>}, EncryptedContent{encrypted_content:String} }`(models.rs:1705)。
- `impl From<ResponseInputItem> for ResponseItem`(models.rs:1486),`McpToolCallOutput` 分支(1506)转成 `ResponseItem::FunctionCallOutput`(1508)。
- `CodexResponsesMetadata`(`core/src/responses_metadata.rs:135`)含 `session_id: String`(137)与 `thread_id: String`(138),均 `pub(crate)`;`stream_responses_api` 入参 `responses_metadata: &CodexResponsesMetadata` 作用域内可达。
- `ApiProvider` = `codex_api::Provider`(`core/src/client.rs:23` `use codex_api::Provider as ApiProvider;`),`#[derive(Clone)]` 非 Copy。
- 借用核实:`&responses_metadata.thread_id` 得 `&String`,传 `queryid: &str` 靠 Deref coercion 自动转换,**不需** `.as_str()`;`&client_setup.api_provider` 短借用在 transform 调用语句末 drop,其后 `api_provider`/`api_auth` 可分别按值 move。

---

## 开发期构建与测试(沿用 llm-switch 决策,2026-06-20)

`zmod/llm-compress` 反向依赖 codex-api/codex-protocol(情况 B,CLAUDE.md §44-63)。cargo 硬约束(已由 llm-switch 实测验证):**非 member 的 path 依赖不能声明 `[dev-dependencies]`、不能跑 `tests/*.rs` 集成测试**(`cargo test -p` 报 `requires dev-dependencies and is not a member`);而 cargo 又拒绝 codex-rs 根之外的 member。**决策:开发期(Task 01–08)用软链把 crate 接进 codex-rs workspace 成为真 member,在 workspace 内编译/测试。**

落地方式(沿用 llm-switch,CLAUDE.md §54-63):

- **软链 member**:`ln -s ../zmod/llm-compress codex-rs/llm-compress`(cargo 视其为根下 member,绕过跨根限制);`codex-rs/Cargo.toml` 的 `members` 末尾加 `"llm-compress",`(软链名,非 `../` 路径);根 `.gitignore` 加 `/codex-rs/llm-compress`。软链就位后 crate 是正式 member,完整支持 `[dev-dependencies]` 与 `tests/*.rs`,共享 codex-rs 的 `Cargo.lock` 与 `target`(无版本漂移、复用已编译 codex-api,快)。
- **测试命令统一**:`cd codex-rs && cargo test -p codez-llm-compress ...`。
- **dev-only 脚手架故意 dirty/未跟踪**:软链 `codex-rs/llm-compress`(gitignore)、`codex-rs/Cargo.toml` 的 members 行、构建产生的 `codex-rs/Cargo.lock` 全程保持 uncommitted,**不得**提交进 codex-rs 子树、**不进** `patches/llm-compress.patch`、**不被还原**。每个任务只提交 `zmod/llm-compress/**`(及 codez 自己的 plans/patches)。
- **生产接入与软链无关**:Task 09 的 patch 走情况 B 的另一条路——`codex-rs/core/Cargo.toml` 加外部 path 依赖 `codez-llm-compress = { path = "../../zmod/llm-compress" }` + client.rs 调用,导出进 `patches/llm-compress.patch`。
- **crate 自身**:`zmod/llm-compress/Cargo.toml` 的 codex-api/codex-protocol 为激活 path 依赖;其余版本对齐 workspace;不声明自己的 `[workspace]`;不提交自己的 `Cargo.lock`(gitignore)。

---

## 任务依赖图

```
01 crate-skeleton-config ─┬─> 02 router-trait ─┬─> 03 truncate ─┐
                          │                    ├─> 04 json ─────┤
                          │                    ├─> 05 diff ─────┼─> 08 transform-entry ─> 09 patch-core
                          │                    └─> 06 log ──────┤
                          └─> 07 stats-log ────────────────────┘
```

- **01** 是地基(crate + config),所有任务依赖它。
- **02** 定义 `Compressor` trait / `Budget` / `ContentRouter`(含 fail-open),是 03–06 的契约。
- **03–06** 四个压缩器互相独立,可并行;均依赖 02 的 trait。
- **07** stats CSV 日志,独立,仅依赖 01 的 crate。
- **08** transform 入口,编排 Layer 0-3 + 遍历 ResponseItem + 文本提取 + 调 router + 调 stats,依赖 02–07。
- **09** patch core(Cargo.toml 依赖 + client.rs 两行)+ 导出 patch + 还原 codex-rs 工作树 + live 验证,依赖 08。

---

## 任务清单

| # | 文件 | 交付物 | spec |
|---|------|--------|------|
| 01 | `2026-06-20-llm-compress-01-crate-skeleton-config.md` | crate 骨架 + `config.rs` 读 `[llm_compress]` | §5/§11 |
| 02 | `2026-06-20-llm-compress-02-router-trait.md` | `Compressor` trait + `Budget` + `ContentRouter`(fail-open) | §4 |
| 03 | `2026-06-20-llm-compress-03-truncate.md` | `TruncateCompressor`(兜底) | §6 |
| 04 | `2026-06-20-llm-compress-04-json.md` | `JsonCompressor`(结构内压缩+parse 校验) | §6 |
| 05 | `2026-06-20-llm-compress-05-diff.md` | `DiffCompressor` | §6 |
| 06 | `2026-06-20-llm-compress-06-log.md` | `LogCompressor` | §6 |
| 07 | `2026-06-20-llm-compress-07-stats-log.md` | `stats.rs` CSV 统计日志 | §7 |
| 08 | `2026-06-20-llm-compress-08-transform-entry.md` | `lib.rs` `transform()` 编排 + ResponseItem 遍历/提取 | §1/§2/§4/§8 |
| 09 | `2026-06-20-llm-compress-09-patch-core.md` | patch core + 导出 patch + live 验证 | §2/§11 |
