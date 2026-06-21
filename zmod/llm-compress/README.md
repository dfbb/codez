# codez-llm-compress

codez 的 zmod 之一。在 codex 把 LLM 请求发往上游**之前**,对已组装好的 `ResponsesApiRequest` 做进程内、不可逆但保守的压缩,降低发往上游的 token 体积,并把每次有效压缩记入 CSV 统计日志。

- **包名**:`codez-llm-compress`(`publish = false`)
- **lib target**:`codez_llm_compress`
- **对应 patch**:`patches/llm-compress.patch`
- **设计依据**:`docs/superpowers/specs/2026-06-20-llm-compress-design.md`

## 它做什么

codex 在 `core/src/client.rs` 的 `stream_responses_api` 里组装好 `ResponsesApiRequest` 后、真正发送前,会调用本 crate 的单一入口 `transform()`。`transform` 就地遍历请求里的**工具输出**文本,按内容类型选一个压缩器压缩超阈的大段文本,把省略处替换成显式占位标记 `[llm-compress: …]`。

它**只**变换请求内容,不换上游、不改响应流、不做可逆检索、不做 token 计数。对下游(原生 `stream_request` / 姊妹 zmod `llm-switch` / SSE 解析 / 错误处理)完全透明:关闭时等价于零改动路径。

与 `llm-switch` 的关系:两者挂在 codex 同一个集成点但职责正交。llm-compress 是**前置拦截**,先压缩后路由,对**所有**请求路径生效(含原生 OpenAI responses 路径),不依赖 llm-switch 是否命中路由。

## 压缩范围(只碰工具输出)

只处理两个 `ResponseItem` 变体的 `output` 文本,其余变体一律不动:

- `FunctionCallOutput`
- `CustomToolCallOutput`

文本提取规则:

- `FunctionCallOutputBody::Text(s)` → 压 `s`。
- `FunctionCallOutputBody::ContentItems(items)` → 逐项**仅压** `InputText{text}` 的 `text`;`InputImage` / `EncryptedContent` 不读不改,绝不 flatten。

## 四个压缩器与路由

内部 `ContentRouter` 按**固定优先级**依次 `detect`,第一个认领的执行 `compress`;`Truncate` 永远兜底。

| 优先级 | 压缩器 | detect 依据 | compress 策略 |
|---|---|---|---|
| ① | **Json** | 能被 `serde_json` 解析为**对象/数组**(顶层标量让渡给下游) | 结构内压缩:长数组抽样(留首尾 + `"…(N more)"`)、超深子树截为 `"…"`;产物必经重新 parse 校验,失败回退原文。绝不做文本级截断。 |
| ② | **Diff** | 含 `@@…@@` hunk 头 / `diff --git` / 成对 `--- ` `+++ ` | 保留全部变更行与结构头,hunk 内多余上下文折叠为 `[llm-compress: 略 N 行上下文]`。 |
| ③ | **Log** | 行数 ≥ 8 且含时间戳 / 栈跟踪 / 连续重复行 | 连续重复行折叠为 `[llm-compress: 上一行 ×N]`;再按 head/tail 保留,中段折叠为 `[llm-compress: 略 N 行]`。 |
| ④ | **Truncate** | 永真(兜底) | 剥 ANSI → 保留 head/tail 行,中间 `[llm-compress: 略 N 行 / M 字节]`;仍超 `max_bytes` 时在 UTF-8 字符边界硬截断。 |

占位标记统一 `[llm-compress: …]`(JSON 例外:占位用合法 JSON 值承载),让模型明确知道此处有省略。压缩**不可逆**,但保守——只压超阈大项,且保证压后体积 ≤ 压前。

## fail-open(压缩永不阻断请求)

`transform()` 返回 `()`(不是 `Result`),从类型上杜绝"压缩失败阻断请求":

- 任一压缩器在 `detect`/`compress` 中 panic → `catch_unwind` 兜住 → 该片段原文透传。
- config 解析失败 → 视作 `enabled = false` + warn,走零改动路径。
- JSON 压后 parse 失败 → 丢弃压缩结果、回退原文(不产出坏 JSON)。
- 统计日志写失败 → 仅 warn,不影响请求。

## 配置

读 `~/.codex/config-zmod.toml` 的 `[llm_compress]` 节(与 llm-switch 同文件、独立节)。**节缺失或 `enabled = false` → 整体关闭**(fail-safe)。`load()` 进程内读一次缓存,不在每个请求上重复读盘。

```toml
[llm_compress]
enabled = false                 # 缺省关闭;置 true 才生效
min_total_bytes = 4096          # 请求工具输出文本总量小于此值 → 整体跳过(小请求不折腾)
per_item_min_bytes = 1024       # 单个文本片段小于此值 → 不压(保守阈值)

[llm_compress.truncate]
head_lines = 50                 # 保留前 N 行
tail_lines = 50                 # 保留后 N 行
max_bytes  = 16384              # 单项压后字节上限,超出则硬截断

[llm_compress.json]
max_array_items = 20            # 数组超此长度 → 抽样保留首尾 + 计数
max_depth = 6                   # 超此深度的子树 → 截为 "…"

[llm_compress.diff]
context_lines = 3               # 每个 hunk 变更行前后保留的上下文行数

[llm_compress.log]
dedup_repeats = true            # 折叠连续重复行为 "[llm-compress: 上一行 ×N]"
```

所有字段都有默认值(见上方注释),缺省即采用。

## 统计日志

- **文件**:`~/.codex/log/llm-compress.log`(目录不存在则创建;append 模式)。
- **触发**:一次请求整体**有效压缩**(`saved_bytes > 0`)后追加一行;关闭 / 未压缩状态不记录。
- **格式**:CSV,四列,无表头,无引号:

  ```
  时间戳,queryid,压缩前字节,压缩后字节
  ```

  ```
  2026-06-20T08:15:30Z,019e3995-5cd9-75a2-b487-f7959835f69e,18432,5120
  ```

| 列 | 来源 |
|---|---|
| 时间戳 | RFC3339 UTC,秒精度(`chrono`) |
| queryid | `responses_metadata.thread_id`(与 rollout 文件名 UUID 精确对应) |
| 压缩前字节 | transform 入口工具输出文本总字节 |
| 压缩后字节 | transform 出口工具输出文本总字节 |

字节口径为工具输出文本字节总和(压缩器的实际作用对象),非整 request 序列化字节。

## 模块布局

```
zmod/llm-compress/
  Cargo.toml
  src/
    lib.rs            # transform() 入口 + enabled() + ResponseItem 遍历/文本提取
    config.rs         # 读 [llm_compress];load() 进程内缓存,load_from() 供测试注入
    stats.rs          # CSV 统计日志 log_compression()
    router.rs         # Compressor trait + Budget + ContentRouter(固定优先级 + fail-open)
    compress/
      mod.rs
      json.rs         # JsonCompressor(结构内压缩 + parse 校验)
      diff.rs         # DiffCompressor(unified diff 上下文折叠)
      log.rs          # LogCompressor(重复行折叠 + head/tail)
      truncate.rs     # TruncateCompressor(兜底:剥 ANSI + head/tail + 硬截断)
  tests/              # 各压缩器 detect/compress + router 优先级/fail-open + transform 端到端
```

## 构建与测试

本 crate 反向依赖 `codex-api` / `codex-protocol`(CLAUDE.md「情况 B」),需软链进 codex-rs workspace 成为真 member 才能跑 `[dev-dependencies]` 与 `tests/*.rs` 集成测试。开发期脚手架(dev-only,不提交进 codex-rs 子树):

```bash
# 在仓库根目录
ln -s ../zmod/llm-compress codex-rs/llm-compress     # 软链(已被 .gitignore 覆盖)
# 在 codex-rs/Cargo.toml 的 [workspace] members 末尾加一行:
#     "llm-compress",

cd codex-rs
cargo test -p codez-llm-compress           # 跑全部测试
cargo clippy -p codez-llm-compress --all-targets
```

软链、`codex-rs/Cargo.toml` 的 member 行、构建生成的 `codex-rs/Cargo.lock` 改动均为 dev-only 脚手架,保持 uncommitted,**不进** `patches/llm-compress.patch`。

## 生产接入(patch)

`patches/llm-compress.patch` 对 codex-rs 的全部侵入(单点、不改任何 codex 函数签名):

1. `codex-rs/core/Cargo.toml` 加外部 path 依赖 `codez-llm-compress = { path = "../../zmod/llm-compress" }`(不进 workspace members)。
2. `codex-rs/core/src/client.rs` 在 `prepare_response_items_for_request` 之后、`record_started` 之前插入 queryid 取值 + `transform()` 调用。

