# Task 10 — ccr.rs:落盘 + Text 占位 + sanitize + 双限

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §4.7。依赖 Task 01(CcrCfg)。可与 05–09 并行(独立模块);Task 11 编排时接入。

**Goal:** 实现 CCR:`RequestCtx` + `CcrRegistry`(每片段一文件)+ `attach`(落盘片段原文、按核心总则只产 Text 占位/或返回原文)。路径组件 sanitize(防 `/`、`..`、超长);双限清理(文件数 + thread 总字节);单文件超限/落盘失败 → 返回原文(保"有损必可取回")。

## Files
- Create: `zmod/llm-compress/src/ccr.rs`
- Modify: `zmod/llm-compress/src/lib.rs`(加 `pub mod ccr;`)
- Test: `zmod/llm-compress/tests/ccr_test.rs`

**Interfaces:**
- Consumes: Task 01 的 `config::CcrCfg`;`sha2`(Task 01 已加依赖)。
- Produces:
  - `pub struct RequestCtx<'a> { pub queryid: &'a str, pub query_terms: Vec<String>, pub cmd_index: std::collections::HashMap<String, crate::command::CommandHint>, pub ccr: std::cell::RefCell<CcrRegistry> }`
  - `pub struct CcrRegistry { ... }`(内部记 (call_id,fragment_hash)→path,默认空;`CcrRegistry::new()`)
  - `pub fn ccr::attach(compressed: String, original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> String`
  - `pub fn ccr::ccr_root() -> Option<std::path::PathBuf>`(`~/.codex/llm-compress/ccr`,供测试注入)

> **核心总则(spec §4.7,写死)**:`enabled=true` 下 attach 只有两种结果——成功落盘→"有损产物+含路径 Text 占位";任何无法落盘(磁盘失败/超 max_file_bytes/路径异常)→**返回原文**(放弃压缩)。`enabled=false`→不落盘、不加路径,返回传入 compressed(保留压缩器自身省略占位)。

---

- [ ] **Step 1: 写失败测试(用 HOME 注入临时目录,沿用 stats_test 模式)**

创建 `zmod/llm-compress/tests/ccr_test.rs`:

```rust
use codez_llm_compress::ccr::{attach, CcrRegistry, RequestCtx};
use codez_llm_compress::config::CcrCfg;
use std::cell::RefCell;
use std::collections::HashMap;

fn ctx<'a>(queryid: &'a str) -> RequestCtx<'a> {
    RequestCtx {
        queryid,
        query_terms: Vec::new(),
        cmd_index: HashMap::new(),
        ccr: RefCell::new(CcrRegistry::new()),
    }
}

fn cfg_enabled() -> CcrCfg {
    CcrCfg { enabled: true, max_files_per_thread: 200, max_thread_bytes: 67_108_864, max_file_bytes: 4_194_304 }
}

#[test]
fn enabled_writes_file_and_appends_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-abc");
    let original = "VERY LONG ORIGINAL CONTENT ".repeat(50);
    let compressed = "[llm-compress: 略 49 行]".to_string();
    let out = attach(compressed.clone(), &original, &c, "call1", &cfg_enabled());
    // 占位里追加了原文路径
    assert!(out.contains("原文:"), "含路径占位");
    assert!(out.starts_with("[llm-compress: 略 49 行]"));
    // 路径指向的文件内容 == 原文
    let path_part = out.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let written = std::fs::read_to_string(path_part).unwrap();
    assert_eq!(written, original);
}

#[test]
fn disabled_returns_compressed_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-x");
    let mut cfg = cfg_enabled();
    cfg.enabled = false;
    let compressed = "[llm-compress: 略 N 行]".to_string();
    let out = attach(compressed.clone(), "original", &c, "call1", &cfg);
    assert_eq!(out, compressed, "disabled:原样返回 compressed,不加路径");
}

#[test]
fn over_max_file_bytes_returns_original() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-y");
    let mut cfg = cfg_enabled();
    cfg.max_file_bytes = 100;
    let original = "x".repeat(500); // > 100
    let compressed = "[llm-compress: 略]".to_string();
    let out = attach(compressed, &original, &c, "call1", &cfg);
    assert_eq!(out, original, "超 max_file_bytes → 返回原文(保有损必可取回)");
}

#[test]
fn sanitizes_unsafe_path_components() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    // queryid 含 / 和 ..,call_id 含 /
    let c = ctx("../../etc/evil");
    let original = "LONG CONTENT ".repeat(50);
    let out = attach("[llm-compress: 略]".to_string(), &original, &c, "a/b/../c", &cfg_enabled());
    let path_part = out.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    // 路径必须落在 HOME/.codex/llm-compress/ccr 下,无穿越
    let root = tmp.path().join(".codex/llm-compress/ccr");
    let canon = std::fs::canonicalize(path_part).unwrap();
    assert!(canon.starts_with(std::fs::canonicalize(&root).unwrap()), "路径不得穿越到 ccr 根外");
}

#[test]
fn same_fragment_reuses_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", tmp.path());
    let c = ctx("thread-z");
    let original = "REPEATED CONTENT ".repeat(50);
    let o1 = attach("[c1]".to_string(), &original, &c, "call1", &cfg_enabled());
    let o2 = attach("[c2]".to_string(), &original, &c, "call1", &cfg_enabled());
    // 同 (call_id, fragment_hash) → 同一文件路径
    let p1 = o1.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    let p2 = o2.split("原文:").nth(1).unwrap().trim().trim_end_matches(']').trim();
    assert_eq!(p1, p2);
}
```

> 注:测试用 `std::env::set_var("HOME", ..)` 注入。`tempfile` 已在 dev-deps。这些测试共享进程环境变量,nextest 默认每测试独立进程,安全;若用 `cargo test` 线程并发可能互相干扰——本 crate 测试用 `cargo test`,故 ccr_test 内多个改 HOME 的测试**可能并发冲突**。缓解:每个测试用唯一 queryid 子目录,断言只查自己写的文件(上面已如此),HOME 指向各自 tempdir(并发时最后设置的 HOME 生效会导致串扰)。**实现期若发现 flaky**,在 ccr_test 顶部加 `use serial_test::serial;` 并给每个测试加 `#[serial]`(需在 Cargo.toml dev-deps 加 `serial-test`),或改用 `ccr_root` 的可注入版本(见 Step 2 备选)。

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test ccr_test 2>&1 | head`
Expected: FAIL(`ccr` 模块不存在)

- [ ] **Step 3: 实现 ccr.rs(类型 + attach 主逻辑)**

创建 `zmod/llm-compress/src/ccr.rs`,先写类型与 attach:

```rust
//! CCR:有损压缩落盘片段原文 + Text 占位写路径,模型用 shell/read 取回(spec §4.7/E)。
//! 核心总则:enabled 下 attach 只产"含路径占位"或"返回原文",绝无"有损但无路径"。

use crate::command::CommandHint;
use crate::config::CcrCfg;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// 一次请求的上下文(Task 11 编排构造)。含可变 CCR registry。
pub struct RequestCtx<'a> {
    pub queryid: &'a str,
    pub query_terms: Vec<String>,
    pub cmd_index: HashMap<String, CommandHint>,
    pub ccr: RefCell<CcrRegistry>,
}

/// 记 (call_id, fragment_hash) → 已落盘文件路径,避免同片段重复落盘。
#[derive(Default)]
pub struct CcrRegistry {
    written: HashMap<(String, String), PathBuf>,
}

impl CcrRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

/// CCR 根目录 ~/.codex/llm-compress/ccr。HOME 未设 → None。
pub fn ccr_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex").join("llm-compress").join("ccr"))
}

/// 落盘片段原文 + 追加 Text 取回占位。仅 lossy=true 项调用。
/// 见 spec §4.7 核心总则:enabled 下要么"含路径占位",要么"返回原文"。
pub fn attach(compressed: String, original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> String {
    if !cfg.enabled {
        return compressed; // disabled:保留压缩器自身占位,不加路径
    }
    // 单文件超限 → 放弃压缩,返回原文(保"有损必可取回")
    if original.len() > cfg.max_file_bytes {
        return original.to_string();
    }
    match try_persist(original, ctx, call_id, cfg) {
        Some(path) => {
            let attached = format!("{compressed} [原文: {}]", path.display());
            // 二次体积检查:占位拼接后若超原文,降级短引用;仍超则返回原文
            if attached.len() <= original.len() {
                attached
            } else {
                let short = format!("{compressed} [llm-compress: 见 ccr]");
                if short.len() <= original.len() {
                    short
                } else {
                    original.to_string()
                }
            }
        }
        None => original.to_string(), // 落盘失败 → 返回原文(不留下不可取回有损产物)
    }
}
```

- [ ] **Step 4: 续写 ccr.rs(try_persist + sanitize + 双限)**

在 `ccr.rs` 末尾追加:

```rust
/// 落盘:sanitize 路径、双限清理、写文件。成功返回路径;任何失败返回 None。
fn try_persist(original: &str, ctx: &RequestCtx, call_id: &str, cfg: &CcrCfg) -> Option<PathBuf> {
    let frag_hash = short_hash(original);
    let key = (call_id.to_string(), frag_hash.clone());
    // 同片段已落盘 → 复用
    if let Some(p) = ctx.ccr.borrow().written.get(&key) {
        return Some(p.clone());
    }
    let root = ccr_root()?;
    let thread_dir = root.join(sanitize_component(ctx.queryid, 64));
    if std::fs::create_dir_all(&thread_dir).is_err() {
        tracing::warn!("llm-compress: ccr mkdir failed");
        return None;
    }
    enforce_limits(&thread_dir, cfg);
    let fname = format!("{}-{}.txt", sanitize_component(call_id, 32), frag_hash);
    let path = thread_dir.join(fname);
    if std::fs::write(&path, original).is_err() {
        tracing::warn!("llm-compress: ccr write failed {path:?}");
        return None;
    }
    ctx.ccr.borrow_mut().written.insert(key, path.clone());
    Some(path)
}

/// SHA256 前 12 hex。
fn short_hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let digest = h.finalize();
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// 路径组件 sanitize:非 [A-Za-z0-9_-] → '_';超 max_len 字节 → 取 SHA256 前 16 hex。
fn sanitize_component(s: &str, max_len: usize) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if cleaned.len() > max_len || cleaned.is_empty() {
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
    } else {
        cleaned
    }
}

/// 双限:文件数超 max_files_per_thread 或目录总字节超 max_thread_bytes → 按 mtime 删最旧。
fn enforce_limits(dir: &std::path::Path, cfg: &CcrCfg) {
    let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let md = e.metadata().ok()?;
                if !md.is_file() {
                    return None;
                }
                let mtime = md.modified().ok()?;
                Some((e.path(), mtime, md.len()))
            })
            .collect(),
        Err(_) => return,
    };
    // 按 mtime 升序(最旧在前)
    entries.sort_by_key(|(_, mtime, _)| *mtime);
    let mut total: u64 = entries.iter().map(|(_, _, sz)| *sz).sum();
    let mut count = entries.len();
    for (path, _, sz) in &entries {
        let over_count = count > cfg.max_files_per_thread;
        let over_bytes = total > cfg.max_thread_bytes;
        if !over_count && !over_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            count -= 1;
            total = total.saturating_sub(*sz);
        }
    }
}
```

在 `lib.rs` 加 `pub mod ccr;`。

- [ ] **Step 5: 运行测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test ccr_test`
Expected: PASS(5 个)。若因并发改 HOME 出现 flaky,按 Step 1 备注加 serial。

- [ ] **Step 6: clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/ccr.rs zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/ccr_test.rs
git commit -m "feat(llm-compress-v2): Task10 ccr.rs 落盘+Text占位+sanitize+双限(核心总则:有损必可取回或返回原文)"
```

