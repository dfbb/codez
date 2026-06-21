# Task 09 — Truncate / Diff Final Verification

> Part of `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §5③ / §5⑥. Depends on Task 01 (signatures already synced) and Task 04 (blob moved to preprocess). Can run in parallel with 05–08.

**Goal:** Confirm that Truncate and Diff behave correctly under the v2 framework: Truncate does **no base64/blob folding** (already moved up to preprocess, Task 04) and produces `lossy=true, kind=Text`; Diff produces `lossy=true, kind=Text` when folding. This task locks down these two points with regression tests and introduces no new algorithm (Task 01 already synced the signatures; v1 truncate never had blob logic, so this only requires assertions to confirm).

## Files
- Modify: `zmod/llm-compress/src/compress/truncate.rs` (only if there really is leftover v1 blob logic to remove; not expected)
- Test: `zmod/llm-compress/tests/truncate_test.rs` (add lossy/kind assertions), `zmod/llm-compress/tests/diff_test.rs` (add lossy/kind assertions)

**Interfaces:**
- Consumes: Task 01's `Budget` / `CompressOutcome` / `ContentKind`.
- Produces: no new interface; locks down the lossy/kind contract for Truncate/Diff.

---

- [ ] **Step 1: Confirm truncate.rs has no base64/blob folding logic**

Run: `grep -n "base64\|blob\|data:" zmod/llm-compress/src/compress/truncate.rs`
Expected: no output (v1 truncate contains no blob; blob folding lives solely in preprocess, spec §4.6 #6). If there is output, delete the relevant branch (it would fold redundantly with preprocess).

- [ ] **Step 2: Write the Truncate lossy/kind regression test**

Append to `zmod/llm-compress/tests/truncate_test.rs`:

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
    assert!(lossy, "truncation drops content → lossy=true");
    assert_eq!(kind, ContentKind::Text);
}
```

> The `cfg_with` / `budget` helpers already exist in truncate_test.rs (Task 01 changed budget to `Budget { cfg, cmd: None, query: &[] }`).

- [ ] **Step 3: Write the Diff lossy/kind regression test**

Append to `zmod/llm-compress/tests/diff_test.rs` (confirming diff folding produces lossy=true, kind=Text):

```rust
#[test]
fn diff_fold_marks_lossy_text_kind() {
    use codez_llm_compress::compress::diff::DiffCompressor;
    use codez_llm_compress::config::Config;
    use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentKind};

    let mut cfg = Config::disabled();
    cfg.diff.context_lines = 1;
    let c = DiffCompressor;
    // Construct a diff with surplus context that can be folded
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
        assert!(lossy, "diff folds context → lossy=true");
        assert_eq!(kind, ContentKind::Text);
    } else {
        panic!("expected compressed diff");
    }
}
```

- [ ] **Step 4: Run the tests and pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test truncate_test --test diff_test`
Expected: PASS (existing cases + 2 new regressions)

- [ ] **Step 5: clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/compress/truncate.rs zmod/llm-compress/tests/truncate_test.rs \
  zmod/llm-compress/tests/diff_test.rs
git commit -m "test(llm-compress-v2): Task09 Truncate/Diff lossy+kind contract regression; confirm blob is not in Truncate"
```
