# codez-llm-compress

在 codex LLM 请求发送边界原地压缩工具调用返回内容,降低 token 消耗。

## 架构概览(v2)

```
transform(request)
  ├─ config::load()            — 读 ~/.codex/config-zmod.toml [llm_compress]
  ├─ ccr::RequestCtx           — 构造一次性请求上下文
  │    ├─ query::extract()     — 抽取当前 query 关键词
  │    └─ command::index()     — 建立 call_id → 命令名 索引
  ├─ build_router()            — Json→Search→Diff→Tabular→Log→Truncate
  └─ per-item compress_in_place()
       ① per_item_min_bytes 阈值门
       ② protect::should_protect()  — 保护门(命中则原文跳过)
       ③ preprocess::run()          — ANSI 清洗 / 进度行去除
       ④ ContentRouter::compress_text() — 路由压缩
       ⑤ ccr::attach()              — lossy 时写 CCR 存档
       ⑥ 体积闸门(只写回≤原文)
```

## 六个压缩器

| 优先级 | 压缩器 | 认领条件 | 方式 |
|--------|--------|----------|------|
| 1 | JsonCompressor | 有效 JSON 对象/数组 | 字段折叠 / 数组截断 |
| 2 | SearchCompressor | grep/rg 风格结果 | 去重上下文行、保留命中行 |
| 3 | DiffCompressor | git diff / unified diff | 折叠大段 context |
| 4 | TabularCompressor | CSV / 固定列表格 | 截断多余行、压缩列 |
| 5 | LogCompressor | 日志行(含时间戳) | 去重连续重复行、折叠低 score |
| 6 | TruncateCompressor | 任意文本(兜底) | 尾部截断到 max_bytes |

## 预处理层

`preprocess::run()` 在路由压缩前对原始字符串做无损或 lossy 清洗:

- ANSI 转义序列剥离(无损)
- 进度条行去除(lossy,计入 CCR)
- 可通过 `[llm_compress.preprocess]` 配置项按需开关

## 命令感知

`command::index()` 从 request 中的 tool 调用历史抽取 call_id → 命令名映射,`Budget.cmd` 把命令名传给各压缩器。压缩器可据此调整压缩策略(如日志压缩器对 `run_tests` 调用保留更多错误行)。

## CCR 取回机制

lossy 压缩后,`ccr::attach()` 把原文写入 `~/.codex/llm-compress/ccr/<queryid>/` 目录,并在压缩结果头部插入取回提示。用户或后续 codex 工具可按需读取原始内容。

CCR 可通过 `[llm_compress.ccr].enabled = false` 关闭。

## 配置

`~/.codex/config-zmod.toml`:

```toml
[llm_compress]
enabled = true
per_item_min_bytes = 512   # 低于此字节数的 item 跳过

[llm_compress.truncate]
max_bytes = 65536

[llm_compress.preprocess]
strip_progress = true       # 删进度条/下载行
collapse_blank = true       # 连续空行归一
truncate_line_bytes = 2000  # 超长单行按字节截断(0=关闭)
dedup_consecutive = true    # 连续重复行折叠为计数
blob_min_bytes = 256        # 超此长度的 base64/blob 行折叠(0=关闭)

[llm_compress.ccr]
enabled = true
max_files_per_thread = 200      # 单线程目录文件数上限
max_thread_bytes = 67108864     # 单线程目录总字节上限(64 MiB)
max_file_bytes = 4194304        # 单个落盘文件上限(4 MiB),超限则放弃压缩返回原文

[llm_compress.protect]
error_max_bytes = 8192      # 含错误标记且小于此字节的输出整段不压缩(0=关闭保护)
```

缺失文件或 table 时,对应功能默认**关闭**(fail-safe)。

## 构建

```bash
# 在 codez-v2 根下
cd codex-rs
cargo build -p codez-llm-compress

# 全量测试(隔离 HOME,避免本地 config-zmod 干扰)
CARGO_HOME=/Users/dfbb/.cargo HOME=$(mktemp -d) cargo test -p codez-llm-compress
```

## 测试结构

| 文件 | 覆盖 |
|------|------|
| `tests/transform_test.rs` | 图片保留等黑盒回归 |
| `tests/orchestration_test.rs` | 端到端编排链 |
| `tests/parity_test.rs` | 继承 fixture 硬不变量(体积不劣、JSON 可解析) |
| `tests/*_test.rs` | 各模块单元测试 |

继承 fixture 来源见 `tests/fixtures/inherited/NOTICE.md`。
