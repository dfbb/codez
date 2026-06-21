# llm-compress v2 实现计划 — 总索引

> **For agentic workers:** REQUIRED SUB-SKILL: 用 superpowers:subagent-driven-development(推荐)或 superpowers:executing-plans 逐任务执行。每个任务是独立 plan 文件,步骤用 `- [ ]` 复选框追踪。

**Goal:** 在已落地的 v1 `codez-llm-compress` 上扩展压缩能力:新增 Search/Tabular 压缩器、升级 Log/JSON、引入 rtk 通用预处理 + 命令感知路由、错误输出保护、CCR(原文取回)。

**Architecture:** 沿用 v1 的 `ContentRouter` + `Compressor` trait + fail-open。`transform` 内先建一次性 `RequestCtx`(查询关键词 + 命令索引 + 可变 CCR registry),再逐片段走"命令识别 → 保护门 → 预处理 → 路由压缩 → CCR 挂载 → 体积闸门"链。不改 transform 签名、不改集成点 patch。

**Tech Stack:** Rust(edition 2021)、serde / serde_json、toml、chrono、tracing、sha2;dev-dep `insta`、`tempfile`。反向 path 依赖 `codex-api` / `codex-protocol`。

**设计依据:** `docs/superpowers/specs/2026-06-21-llm-compress-v2-design.md`(已定稿,经六轮评审)。每个任务标注覆盖的 spec 小节。

---

## Global Constraints

逐条照抄自 spec §1,每个任务隐含包含本节:

- **不改 transform 签名**:`pub fn transform(request: &mut ResponsesApiRequest, _api_provider: &ApiProvider, queryid: &str)`,返回 `()`。新能力的输入全从 `request` 内提取。
- **不改集成点 patch**:不新增 `core/src/client.rs` 触点,不碰 codex 工具系统。
- **只处理两个变体**:`FunctionCallOutput` / `CustomToolCallOutput` 的 `output`。`Text(s)` 压 `s`;`ContentItems` 仅压 `InputText.text`,图片/加密内容不读不改,绝不 flatten。
- **fail-open 贯穿**:任何环节出问题退回原文/跳过,绝不阻断请求;非测试代码不用 `unwrap`/`expect`(`catch_unwind` 边界除外)。
- **占位标记统一** `[llm-compress: …]`。**压后体积 ≤ 压前**(两道闸门:`ccr::attach` 内 + 编排层最终写回)。UTF-8 安全。
- **lossy 语义口径(spec §4.0)**:`lossy=true` ⟺ 删了实质内容。纯格式重构(JSON minify/csv-schema/表格转 JSON/连续空行归一/连续重复 RLE)`lossy=false` 不挂 CCR;抽样/删行/删匹配/截断/blob 折叠 `lossy=true` 挂 CCR。
- **两条铁律不变量**:`kind=Json ⟹ lossy=false`(JSON/Tabular 永不挂 CCR);`lossy=true ⟹ kind=Text`(`attach` 只产 Text 裸占位、不收 kind、无 JSON 注入)。
- **CCR 核心总则(spec §4.7)**:`ccr.enabled=true` 下"有损与可取回绑定"——`attach` 结果只有"成功落盘+含路径占位"或"返回原文(放弃压缩)",**绝不出现"有损产物但无可取回路径"**。仅 `enabled=false` 允许"有损但不可取回"。
- **配置 fail-safe**:`~/.codex/config-zmod.toml` 无 `[llm_compress]` 节或 `enabled=false` → 整体关闭、零改动路径。

---

## 开发期构建与测试(继承 v1,CLAUDE.md 情况 B)

`zmod/llm-compress` 反向依赖 codex-api/codex-protocol,需软链进 codex-rs workspace 成 member 才能跑 `[dev-dependencies]` + `tests/*.rs`。dev-only 脚手架(不提交进 codex-rs 子树、不进 patch):

```bash
# 在仓库根目录,若软链/member 未就位则重建(git reset --hard 会撤 members 行,软链被 gitignore 而留存)
ln -s ../zmod/llm-compress codex-rs/llm-compress 2>/dev/null || true
# 确认 codex-rs/Cargo.toml 的 [workspace] members 含 "llm-compress"(没有则在 members 数组首行后加一行 "llm-compress",)
grep -q '"llm-compress"' codex-rs/Cargo.toml || echo '需手动在 codex-rs/Cargo.toml members 加 "llm-compress",'
```

**所有任务的测试命令统一**:`cd codex-rs && cargo test -p codez-llm-compress`(或 `--test <name>` 跑单文件)。`cargo clippy -p codez-llm-compress --all-targets` 做 lint。

---

## 任务依赖图

```
01 接口地基(router+共享类型骨架+config+schema) ─┬─> 05 JSON 升级
02 query + score ───────────────────────────────┼─> 06 Search
03 command ─────────────────────────────────────┼─> 07 Tabular
04 preprocess + protect ────────────────────────┼─> 08 Log 改写
                                                 └─> 09 Truncate/Diff 收尾
10 ccr ──────────────────────────────────────────────> 11 lib 编排 + fixture + parity
```

- **01** 是接口地基:定义全部跨模块共享类型(`ContentKind`/`CompressOutcome`/`Budget`/`CommandHint`/`RequestCtx`/`CcrRegistry`)、改 `Compressor` trait + `compress_text` + config 全量扩展 + `schema.rs`,并**同步现有 4 压缩器签名**使 crate 编译通过。所有后续任务依赖它。
- **02–04** 共享原语/模块,依赖 01 的类型,互相独立可并行。
- **05–09** 各压缩器,依赖 01–04,互相独立可并行。
- **10** ccr,依赖 01 的 `RequestCtx`/`CcrRegistry`/`CcrCfg`。
- **11** lib 编排把全部接线 + 继承 fixture + parity_test 收尾,依赖全部。

---

## 任务清单

| # | 文件 | 交付物 | spec |
|---|------|--------|------|
| 01 | `2026-06-21-llm-compress-v2-01-foundation.md` | router 接口 + 共享类型 + config 扩展 + schema.rs + 现有压缩器签名同步 | §4.0/§4.1/§4.8/§6 |
| 02 | `2026-06-21-llm-compress-v2-02-query-score.md` | `query.rs` + `score.rs` 共享原语 | §4.2/§4.4 |
| 03 | `2026-06-21-llm-compress-v2-03-command.md` | `command.rs` call_id→CommandHint 索引 | §4.3 |
| 04 | `2026-06-21-llm-compress-v2-04-preprocess-protect.md` | `preprocess.rs`(含 blob_fold)+ `protect.rs` | §4.5/§4.6 |
| 05 | `2026-06-21-llm-compress-v2-05-json.md` | JSON 升级:detect 让位 + RLE + csv-schema | §5① |
| 06 | `2026-06-21-llm-compress-v2-06-search.md` | `search.rs` SearchCompressor | §5② |
| 07 | `2026-06-21-llm-compress-v2-07-tabular.md` | `tabular.rs` TabularCompressor | §5④ |
| 08 | `2026-06-21-llm-compress-v2-08-log.md` | Log 改写:模板挖掘 + 级别评分 | §5⑤ |
| 09 | `2026-06-21-llm-compress-v2-09-truncate-diff.md` | Truncate 去 blob + Diff lossy 标记收尾 | §5③/§5⑥ |
| 10 | `2026-06-21-llm-compress-v2-10-ccr.md` | `ccr.rs` 落盘 + 占位 + sanitize + 双限 | §4.7 |
| 11 | `2026-06-21-llm-compress-v2-11-orchestration-parity.md` | lib 编排接线 + 继承 fixture + parity_test | §2/§8/§9 |

## 成功判据(全计划完成后,对照 spec §9.2)

1. 六压缩器 + 预处理 + 保护门 + CCR 全部落地,路由优先级 `Json→Search→Diff→Tabular→Log→Truncate` 生效,命令提示能重排候选。
2. 继承 fixture 的 `parity_test` 全绿(§8.3 四类不变量)。
3. 硬不变量满足:压后 ≤ 压前、UTF-8 合法、JSON 产物可 parse、只碰两变体、图片不动、`enabled=false` 逐字节不变。
4. CCR `enabled=true` 下有损必可取回;`enabled=false` 不要求;parity 固定 enabled 下跑。
5. transform 签名与集成点 patch 不变。
6. fail-open 全覆盖。
