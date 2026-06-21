# Task 09 — Truncate / Diff 收尾验证

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §5③ / §5⑥。依赖 Task 01(签名已同步)、Task 04(blob 归 preprocess)。可与 05–08 并行。

**Goal:** 确认 Truncate 与 Diff 在 v2 框架下行为正确:Truncate **不做 base64/blob 折叠**(已上移 preprocess,Task 04)、产 `lossy=true,kind=Text`;Diff 折叠时 `lossy=true,kind=Text`。本任务以回归测试锁定这两点,不引入新算法(Task 01 已同步签名;v1 truncate 本就无 blob 逻辑,只需断言确认)。

## Files
- Modify: `zmod/llm-compress/src/compress/truncate.rs`(仅在确有 v1 残留 blob 逻辑时移除;预期无)
- Test: `zmod/llm-compress/tests/truncate_test.rs`(追加 lossy/kind 断言)、`zmod/llm-compress/tests/diff_test.rs`(追加 lossy/kind 断言)

**Interfaces:**
- Consumes: Task 01 的 `Budget`/`CompressOutcome`/`ContentKind`。
- Produces: 无新接口,锁定 Truncate/Diff 的 lossy/kind 契约。

---

- [ ] **Step 1: 确认 truncate.rs 无 base64/blob 折叠逻辑**

Run: `grep -n "base64\|blob\|data:" zmod/llm-compress/src/compress/truncate.rs`
Expected: 无输出(v1 truncate 不含 blob;blob 折叠唯一在 preprocess,spec §4.6 #6)。若有输出,删除相关分支(它会与 preprocess 重复折叠)。

- [ ] **Step 2: 写 Truncate lossy/kind 回归测试**

向 `zmod/llm-compress/tests/truncate_test.rs` 追加:

```rust
#[test]
fn truncate_marks_lossy_text_kind() {
    use codez_llm_compress::router::ContentKind;
    let cfg = cfg_with(2, 2, 1_000_000);
    let lines: Vec<String> = (0..200).map(|i| format!("line{i:04}_payload_xxxxxxxxxxxxxxxxxxxx")).collect();
    let input = lines.join("\n");
    let out = TruncateCompressor.compress(&input, &budget(&cfg));
    let CompressOutcome::Compressed { lossy, kind, .. } = out else {
        panic!("expected Compressed");
    };
    assert!(lossy, "截断删内容 → lossy=true");
    assert_eq!(kind, ContentKind::Text);
}
```

> `cfg_with` / `budget` helper 在 truncate_test.rs 已存在(Task 01 已把 budget 改为 `Budget { cfg, cmd: None, query: &[] }`)。

- [ ] **Step 3: 写 Diff lossy/kind 回归测试**

向 `zmod/llm-compress/tests/diff_test.rs` 追加(确认 diff 折叠产 lossy=true,kind=Text):

```rust
#[test]
fn diff_fold_marks_lossy_text_kind() {
    use codez_llm_compress::compress::diff::DiffCompressor;
    use codez_llm_compress::config::Config;
    use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

    let mut cfg = Config::disabled();
    cfg.diff.context_lines = 1;
    let c = DiffCompressor;
    // 构造一个有多余上下文可折叠的 diff
    let mut lines = vec!["diff --git a/f b/f".to_string(), "--- a/f".to_string(), "+++ b/f".to_string(), "@@ -1,20 +1,20 @@".to_string()];
    for i in 0..10 {
        lines.push(format!(" ctx{i}"));
    }
    lines.push("-old".to_string());
    lines.push("+new".to_string());
    for i in 0..10 {
        lines.push(format!(" ctx{}", 10 + i));
    }
    let text = lines.join("\n");
    let b = Budget { cfg: &cfg, cmd: None, query: &[] };
    if let CompressOutcome::Compressed { lossy, kind, .. } = c.compress(&text, &b) {
        assert!(lossy, "diff 折叠上下文 → lossy=true");
        assert_eq!(kind, ContentKind::Text);
    } else {
        panic!("expected compressed diff");
    }
}
```

- [ ] **Step 4: 运行测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test truncate_test --test diff_test`
Expected: PASS(现有用例 + 2 个新回归)

- [ ] **Step 5: clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/compress/truncate.rs zmod/llm-compress/tests/truncate_test.rs \
  zmod/llm-compress/tests/diff_test.rs
git commit -m "test(llm-compress-v2): Task09 Truncate/Diff lossy+kind 契约回归;确认 blob 不在 Truncate"
```
