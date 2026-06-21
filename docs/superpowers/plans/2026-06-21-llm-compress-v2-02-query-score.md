# Task 02 — 共享原语:query.rs + score.rs

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §4.2 / §4.4。依赖 Task 01(无类型依赖,可与 03/04 并行)。

**Goal:** 实现两个纯函数共享原语:`query::extract` 从 request 提取最后一条 user 消息的关键词;`score::line_score` 给单行打分(内容特征 + 查询加权)。供 Search(Task 06)、Log(Task 08)使用。

## Files
- Create: `zmod/llm-compress/src/query.rs`
- Create: `zmod/llm-compress/src/score.rs`
- Modify: `zmod/llm-compress/src/lib.rs`(加 `pub mod query; pub mod score;`)
- Test: `zmod/llm-compress/tests/query_test.rs`、`zmod/llm-compress/tests/score_test.rs`

**Interfaces:**
- Consumes: 无(纯函数)。codex 类型:`codex_api::ResponsesApiRequest`、`codex_protocol::models::{ResponseItem, ContentItem}`。
- Produces:
  - `pub fn query::extract(request: &codex_api::ResponsesApiRequest) -> Vec<String>`
  - `pub fn score::line_score(line: &str, query: &[String]) -> f32`

---

- [ ] **Step 1: 写 query 失败测试**

创建 `zmod/llm-compress/tests/query_test.rs`:

```rust
use codez_llm_compress::query::extract;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{ContentItem, ResponseItem};

fn req(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "m".to_string(),
        instructions: String::new(),
        input,
        tools: Vec::new(),
        tool_choice: "auto".to_string(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

fn user_msg(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: text.to_string() }],
        phase: None,
        metadata: None,
    }
}

#[test]
fn extracts_keywords_from_last_user_message() {
    let r = req(vec![
        user_msg("first old request about parsing"),
        user_msg("fix the failing database connection timeout"),
    ]);
    let kw = extract(&r);
    // 取最后一条 user;去停用词 the；去长度<=2；小写
    assert!(kw.contains(&"failing".to_string()));
    assert!(kw.contains(&"database".to_string()));
    assert!(kw.contains(&"connection".to_string()));
    assert!(!kw.contains(&"the".to_string()));
}

#[test]
fn no_user_message_returns_empty() {
    let r = req(vec![ResponseItem::Other]);
    assert!(extract(&r).is_empty());
}

#[test]
fn empty_input_returns_empty() {
    let r = req(vec![]);
    assert!(extract(&r).is_empty());
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test query_test 2>&1 | head`
Expected: FAIL(`query` 模块不存在 / 编译错误)

- [ ] **Step 3: 实现 query.rs**

创建 `zmod/llm-compress/src/query.rs`:

```rust
//! 从 request 提取"最后一条 user 消息"的关键词,供 score 做查询加权(spec §4.2/S2)。

use codex_api::ResponsesApiRequest;
use codex_protocol::models::{ContentItem, ResponseItem};

const MAX_TERMS: usize = 32;
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "are", "was",
    "you", "your", "but", "not", "all", "can", "has", "have", "will", "what",
];

/// 从后往前找第一条 role=="user" 的 Message,取其 InputText 文本,分词成关键词。
/// 找不到 → 空 Vec(评分退化为纯内容特征,不报错)。
pub fn extract(request: &ResponsesApiRequest) -> Vec<String> {
    let text = request.input.iter().rev().find_map(|item| match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            let mut s = String::new();
            for c in content {
                if let ContentItem::InputText { text } = c {
                    s.push_str(text);
                    s.push(' ');
                }
            }
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        _ => None,
    })?;
    tokenize(&text)
}

fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.len() <= 2 {
            continue;
        }
        let w = raw.to_lowercase();
        if STOPWORDS.contains(&w.as_str()) {
            continue;
        }
        if seen.insert(w.clone()) {
            out.push(w);
            if out.len() >= MAX_TERMS {
                break;
            }
        }
    }
    out
}
```

> 注意:`extract` 用 `?` 在 `find_map` 返回 `None` 时直接返回——但 `Vec` 不是 `Option`。改用显式 match:

实际实现把末尾改为:
```rust
    match text {
        Some(t) => tokenize(&t),
        None => Vec::new(),
    }
}
```
并把 `find_map(...)?;` 改为 `let text: Option<String> = request.input.iter().rev().find_map(...);`(去掉 `?`,绑定为 Option)。

在 `lib.rs` 加 `pub mod query;`。

- [ ] **Step 4: 运行 query 测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test query_test`
Expected: PASS(3 个)

- [ ] **Step 5: 写 score 失败测试**

创建 `zmod/llm-compress/tests/score_test.rs`:

```rust
use codez_llm_compress::score::line_score;

#[test]
fn error_lines_score_high() {
    let q: Vec<String> = vec![];
    assert!(line_score("ERROR: something panicked", &q) >= 1.0);
    assert!(line_score("thread panicked at foo.rs:42", &q) >= 1.0);
    assert!(line_score("  Traceback (most recent call last):", &q) >= 1.0);
}

#[test]
fn warning_lines_score_medium() {
    let q: Vec<String> = vec![];
    let s = line_score("warning: unused variable x", &q);
    assert!(s >= 0.5 && s < 1.0);
}

#[test]
fn plain_lines_score_low() {
    let q: Vec<String> = vec![];
    assert!(line_score("just a normal line of output", &q) < 0.5);
    assert_eq!(line_score("", &q), 0.0);
    assert_eq!(line_score("   ", &q), 0.0);
}

#[test]
fn query_terms_add_weight() {
    let q = vec!["database".to_string(), "timeout".to_string()];
    let with = line_score("connecting to database", &q);
    let without = line_score("connecting to server", &q);
    assert!(with > without);
}
```

- [ ] **Step 6: 运行确认失败**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test score_test 2>&1 | head`
Expected: FAIL(`score` 模块不存在)

- [ ] **Step 7: 实现 score.rs**

创建 `zmod/llm-compress/src/score.rs`:

```rust
//! 单行评分:内容特征 + 查询关键词加权(spec §4.4/S1)。纯静态,无 ML。
//! 供 Search/Log 决定保留哪些行。

const ERROR_KEYWORDS: &[&str] = &["error", "fail", "panic", "exception", "traceback", "fatal"];
const WARN_KEYWORDS: &[&str] = &["warn", "warning"];

/// 行得分:错误关键词 +1.0;警告 +0.5;查询词命中每个 +0.3;空行/纯符号 0。
pub fn line_score(line: &str, query: &[String]) -> f32 {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return 0.0;
    }
    let lower = line.to_lowercase();
    let mut score = 0.0_f32;
    if ERROR_KEYWORDS.iter().any(|k| lower.contains(k)) {
        score += 1.0;
    } else if WARN_KEYWORDS.iter().any(|k| lower.contains(k)) {
        score += 0.5;
    }
    for q in query {
        if lower.contains(q.as_str()) {
            score += 0.3;
        }
    }
    score
}
```

在 `lib.rs` 加 `pub mod score;`。

- [ ] **Step 8: 运行 score 测试通过**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test score_test`
Expected: PASS(4 个)

- [ ] **Step 9: 全量编译 + clippy + 提交**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: 全绿、无 warning

```bash
git add zmod/llm-compress/src/query.rs zmod/llm-compress/src/score.rs \
  zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/query_test.rs zmod/llm-compress/tests/score_test.rs
git commit -m "feat(llm-compress-v2): Task02 共享原语 query.rs + score.rs"
```

