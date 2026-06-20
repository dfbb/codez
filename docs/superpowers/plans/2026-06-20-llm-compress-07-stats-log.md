# Task 07: stats.rs CSV 压缩统计日志

> 属于 `2026-06-20-llm-compress-00-index.md`。执行前先读 index 的 Global Constraints / 真实类型 / dev-build 决策。
>
> 本任务**独立**,只依赖 Task 01 的 crate 骨架(crate 已在 codex-rs workspace 内,`chrono` 已是依赖)。可与 02–06 并行。

**Goal:** 实现 `stats.rs`——把一次有效压缩的统计写一行 CSV 到 `~/.codex/log/llm-compress.log`(append,目录不存在则建)。提供两个钉死的公共函数:对外的 `log_compression`(解析路径 + 取当前 UTC 时间)与可注入的纯函数式 `log_compression_to`(给定 path + 时间戳字符串)。fail-open:写失败只 `tracing::warn!`,绝不 panic、绝不返回 Err 阻断上层。Task 08 的 transform 在判断"整体有效压缩(`saved_bytes>0`)"后调用本模块,**是否触发由调用方判断**,本模块只负责"写一行"。

**覆盖 spec:** §7(统计日志)。

**Files:**
- Create: `zmod/llm-compress/src/stats.rs`
- Create: `zmod/llm-compress/tests/stats_test.rs`
- Modify: `zmod/llm-compress/src/lib.rs`(加 `pub mod stats;`)

**Interfaces:**
- Produces(Task 08 依赖,签名钉死,不可改):
  ```rust
  /// 写一行压缩统计到 ~/.codex/log/llm-compress.log。失败仅 warn,不 panic。
  pub fn log_compression(queryid: &str, before: usize, after: usize);
  /// 测试可注入路径与时间戳的内部版本。
  pub fn log_compression_to(path: &std::path::Path, timestamp_rfc3339: &str, queryid: &str, before: usize, after: usize) -> std::io::Result<()>;
  ```
- Consumes:`chrono`(Task 01 已加 `chrono = { version = "0.4", features = ["clock"] }`)、`tempfile`(Task 01 已加 dev-dep)。

**格式规格(严格):** CSV 四列,**无表头,无引号**:`时间戳,queryid,压缩前字节,压缩后字节`。时间戳为 RFC3339 UTC(秒精度,形如 `...Z`)。示例行:
```
2026-06-20T08:15:30Z,019e3995-5cd9-75a2-b487-f7959835f69e,18432,5120
```

---

- [ ] **Step 1: 写失败测试**

Create `zmod/llm-compress/tests/stats_test.rs`:

```rust
//! stats.rs 测试:用 tempfile 注入路径、固定时间戳字符串,
//! 验证精确行格式、append 语义、父目录自动创建、CSV 四列无引号无表头。

use codez_llm_compress::stats::log_compression_to;
use std::fs;

#[test]
fn writes_exact_single_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    // 读回断言精确等于(含尾部换行)
    assert_eq!(content, "2026-06-20T08:15:30Z,abc,100,40\n");
}

#[test]
fn appends_second_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();
    log_compression_to(&path, "2026-06-20T09:00:00Z", "def", 200, 80).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "2026-06-20T08:15:30Z,abc,100,40");
    assert_eq!(lines[1], "2026-06-20T09:00:00Z,def,200,80");
}

#[test]
fn creates_missing_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    // tempdir 下一个尚不存在的子目录
    let path = dir.path().join("nested").join("deeper").join("llm-compress.log");
    assert!(!path.parent().unwrap().exists());

    log_compression_to(&path, "2026-06-20T08:15:30Z", "abc", 100, 40).unwrap();

    assert!(path.exists());
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "2026-06-20T08:15:30Z,abc,100,40\n");
}

#[test]
fn line_format_is_four_columns_no_header_no_quotes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("llm-compress.log");

    log_compression_to(
        &path,
        "2026-06-20T08:15:30Z",
        "019e3995-5cd9-75a2-b487-f7959835f69e",
        18432,
        5120,
    )
    .unwrap();

    let content = fs::read_to_string(&path).unwrap();
    // 无引号
    assert!(!content.contains('"'));
    // 单行(无表头),逗号分隔恰好 4 列
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1);
    let cols: Vec<&str> = lines[0].split(',').collect();
    assert_eq!(cols.len(), 4);
    assert_eq!(cols[0], "2026-06-20T08:15:30Z");
    assert_eq!(cols[1], "019e3995-5cd9-75a2-b487-f7959835f69e");
    assert_eq!(cols[2], "18432");
    assert_eq!(cols[3], "5120");
}
```

- [ ] **Step 2: 跑测试看失败**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test stats_test
```
Expected: 编译错误 `unresolved import codez_llm_compress::stats`(Step 3/4 尚未实现)。

- [ ] **Step 3: 写 stats.rs 完整实现**

Create `zmod/llm-compress/src/stats.rs`:

```rust
//! 压缩统计 CSV 日志(spec §7)。
//!
//! 仅当一次请求整体有效压缩(saved_bytes>0)时,Task 08 的 transform 会调用
//! `log_compression` 写一行。本模块只负责"写一行",是否触发由调用方判断。
//!
//! 格式:CSV 四列,无表头,无引号:`时间戳,queryid,压缩前字节,压缩后字节`。
//! 时间戳为 RFC3339 UTC,秒精度,形如 `2026-06-20T08:15:30Z`。
//!
//! fail-open:写日志失败(目录建不了 / 权限 / 磁盘满)只记一条 `tracing::warn!`,
//! 绝不 panic、绝不返回 Err 阻断上层压缩流程。

use chrono::SecondsFormat;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 默认日志路径:`~/.codex/log/llm-compress.log`(用 HOME 环境变量解析)。
fn default_log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".codex")
            .join("log")
            .join("llm-compress.log"),
    )
}

/// 写一行压缩统计到 ~/.codex/log/llm-compress.log。失败仅 warn,不 panic。
///
/// 内部:解析默认路径、取当前 UTC 时间格式化为 RFC3339(秒精度,`...Z`),
/// 委托 `log_compression_to`;后者的 `Err` 在此被转成 `tracing::warn!` 吞掉。
pub fn log_compression(queryid: &str, before: usize, after: usize) {
    let path = match default_log_path() {
        Some(p) => p,
        None => {
            tracing::warn!("llm-compress: HOME unset, skip stats log");
            return;
        }
    };
    // RFC3339,UTC,秒精度,带 Z(use_z=true)
    let ts = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    if let Err(e) = log_compression_to(&path, &ts, queryid, before, after) {
        tracing::warn!("llm-compress: failed to write stats log {path:?}: {e}");
    }
}

/// 测试可注入路径与时间戳的内部版本。
///
/// 纯函数式:给定 path + 时间戳字符串。必要时 `create_dir_all` 父目录,
/// 以 append+create 模式打开,写 `format!("{ts},{qid},{before},{after}\n")`。
pub fn log_compression_to(
    path: &Path,
    timestamp_rfc3339: &str,
    queryid: &str,
    before: usize,
    after: usize,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    let line = format!("{timestamp_rfc3339},{queryid},{before},{after}\n");
    file.write_all(line.as_bytes())?;
    Ok(())
}
```

> 实现要点核对:
> - `to_rfc3339_opts(SecondsFormat::Secs, true)` 给出形如 `2026-06-20T08:15:30+00:00` 的 UTC——`use_z=true` 时输出 `Z` 后缀,即 `2026-06-20T08:15:30Z`,满足"秒精度 / `...Z`"。
> - `OpenOptions::append(true).create(true)`:文件不存在则建、存在则追加,**不** `truncate`。
> - `path.parent()` 为 `None` 的退化情形(纯文件名,无父目录)直接跳过建目录;实际默认路径恒有父目录。
> - 写入失败一律以 `Err` 返回给 `log_compression`,由其 warn 吞掉——`log_compression_to` 自身不 warn、不 panic。

- [ ] **Step 4: lib.rs 注册模块**

Modify `zmod/llm-compress/src/lib.rs`,新增一行模块声明(与既有 `pub mod config;` 同级):

```rust
pub mod stats;
```

> 仅加这一行,不动其它内容。

- [ ] **Step 5: 跑测试看通过**

Run(在 `codex-rs/` 目录):
```bash
cargo test -p codez-llm-compress --test stats_test
```
Expected: `test result: ok. 4 passed`。

- [ ] **Step 6: 提交(仅 zmod + 本计划文件)**

```bash
cd /Users/dfbb/Sites/skycode/codez
git add zmod/llm-compress docs/superpowers/plans/2026-06-20-llm-compress-07-stats-log.md
git -c core.hooksPath=/dev/null commit -m "feat(llm-compress): CSV compression stats log"
```

> **不要** `git add codex-rs/...`——dev-build 使能器(core/Cargo.toml 的 dirty 行)保持 uncommitted 至 Task 09。
