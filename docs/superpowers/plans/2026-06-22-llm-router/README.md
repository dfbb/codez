# llm-router(purpose routing)实现计划 — 索引

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 `zmod/llm-switch` 增加「按用途(compact/review/memory)路由到不同模型后端」的能力,主模型行为不变。

**Architecture:** 在 llm-switch crate 内新增 `Purpose` 枚举、`purpose_from_source`(SessionSource→Purpose)、`request_has_namespace_tools`(请求不可表达工具预检)、两级 `route()`(purpose→provider-id→原生)与 `should_bypass_websocket`(传输层绕过判定);codex 侧改动全部表达在 `patches/llm-switch.patch`,不直接改 `codex-rs` 源码。

**Tech Stack:** Rust 1.95.0、cargo nextest/test、wiremock(dev)、codex-api / codex-protocol(path 反向依赖)。

设计来源:`docs/superpowers/specs/2026-06-22-llm-switch-purpose-routing-design.md`(本计划逐节对应该 spec)。

## Global Constraints

- Rust 工具链固定 `1.95.0`(`codex-rs/rust-toolchain.toml`)。
- **不直接修改 `codex-rs/` 源码**;对 codex 的侵入式改动只能写进 `patches/llm-switch.patch`(codez zmod ↔ patch 约定)。
- **fail-safe**:配置缺失 / 坏映射 / 读不到配置时一律不 panic、不报错,按「该用途未启用」处理。
- 回退唯一权威口径:**purpose → provider-id → 原生**,沿链下落,绝不越级跳原生(spec §4 钉死语义)。
- 非测试代码避免 `unwrap`/`expect`;测试代码允许。
- crate 名 `codez-llm-switch`,lib target `codez_llm_switch`;不新建 crate、不新建 patch。
- 开发期测试依赖软链 + member 脚手架(见 Task 1 Step 1),**该脚手架 dev-only,绝不提交**(软链已在根 `.gitignore`;members 行与 `codex-rs/Cargo.lock` 保持 uncommitted dirty)。

## 文件结构(决策锁定)

| 文件 | 责任 | Task |
|---|---|---|
| `zmod/llm-switch/src/config.rs`(改) | 解析 `[llm-switch.purpose]` 映射表,`Config` 增 `purpose` 字段 | 1 |
| `zmod/llm-switch/src/purpose.rs`(新) | `Purpose` 枚举 + `purpose_from_source` + `Purpose::as_key` | 2 |
| `zmod/llm-switch/src/namespace.rs`(新) | `request_has_namespace_tools` 请求预检 | 3 |
| `zmod/llm-switch/src/lib.rs`(改) | 两级 `route()` 升级签名 + `should_bypass_websocket` + 模块接线/导出 | 4 |
| `patches/llm-switch.patch`(改) | codex 侧集成:route 调用点、source override、memory 调用点、WS 绕过 | 5 |

依赖顺序:1 → 2 → 3 → 4 → 5(每个 task 末尾产物可独立测试/审查)。Task 2、3 各自在 lib.rs 添加自己的 `mod`+`pub use`(模块注册),Task 4 才写 route/bypass 逻辑。

## Task 列表(一 task 一文件)

1. [Task 1: config.rs — purpose 映射表解析](task-01-config-purpose-table.md)
2. [Task 2: purpose.rs — Purpose 与 purpose_from_source](task-02-purpose-mapping.md)
3. [Task 3: namespace.rs — request_has_namespace_tools 预检](task-03-namespace-predicate.md)
4. [Task 4: lib.rs — 两级 route() 与 should_bypass_websocket](task-04-route-and-bypass.md)
5. [Task 5: patches/llm-switch.patch — codex 侧集成](task-05-patch-integration.md)

## Execution Handoff

见各 task 文件。Task 1-4 在 `codez-llm-switch` crate 内 TDD,`cd codex-rs && cargo test -p codez-llm-switch` 验证;Task 5 用 `git apply --check` + 打 patch 后 `cargo build -p codex-core` 验证。
