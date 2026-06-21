# Task 02 — Shared primitives: query.rs + score.rs

> Belongs to `2026-06-21-llm-compress-v2-00-index.md`. Covers spec §4.2 / §4.4. Depends on Task 01 (no type dependency; can run in parallel with 03/04).

**Goal:** Implement two pure-function shared primitives: `query::extract` extracts keywords from the last user message in a request; `score::line_score` scores a single line (content features + query weighting). Used by Search (Task 06) and Log (Task 08).

## Files
- Create: `zmod/llm-compress/src/query.rs`
- Create: `zmod/llm-compress/src/score.rs`
- Modify: `zmod/llm-compress/src/lib.rs` (add `pub mod query; pub mod score;`)
- Test: `zmod/llm-compress/tests/query_test.rs`, `zmod/llm-compress/tests/score_test.rs`

**Interfaces:**
- Consumes: none (pure functions). codex types: `codex_api::ResponsesApiRequest`, `codex_protocol::models::{ResponseItem, ContentItem}`.
- Produces:
  - `pub fn query::extract(request: &codex_api::ResponsesApiRequest) -> Vec<String>`
  - `pub fn score::line_score(line: &str, query: &[String]) -> f32`

---

- [ ] **Step 1: Write the failing query test**

Create `zmod/llm-compress/tests/query_test.rs`:

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
    // Take the last user message; drop the stopword "the"; drop length <= 2; lowercase
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

- [ ] **Step 2: Run and confirm it fails**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test query_test 2>&1 | head`
Expected: FAIL (`query` module does not exist / compile error)

- [ ] **Step 3: Implement query.rs**

Create `zmod/llm-compress/src/query.rs`:

```rust
//! Extracts keywords from the "last user message" in a request, for score's query weighting (spec §4.2/S2).

use codex_api::ResponsesApiRequest;
use codex_protocol::models::{ContentItem, ResponseItem};

const MAX_TERMS: usize = 32;
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "are", "was",
    "you", "your", "but", "not", "all", "can", "has", "have", "will", "what",
];

/// Scan from the back, find the first Message with role == "user", take its InputText
/// text, and tokenize it into keywords.
/// Not found -> empty Vec (scoring degrades to pure content features, no error).
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

> Note: `extract` uses `?` to return directly when `find_map` yields `None` — but `Vec` is not `Option`. Use an explicit match instead:

The actual implementation changes the tail to:
```rust
    match text {
        Some(t) => tokenize(&t),
        None => Vec::new(),
    }
}
```
And changes `find_map(...)?;` to `let text: Option<String> = request.input.iter().rev().find_map(...);` (drop the `?`, bind as an Option).

Add `pub mod query;` in `lib.rs`.

- [ ] **Step 4: Run the query test and pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test query_test`
Expected: PASS (3 tests)

- [ ] **Step 5: Write the failing score test**

Create `zmod/llm-compress/tests/score_test.rs`:

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

- [ ] **Step 6: Run and confirm it fails**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test score_test 2>&1 | head`
Expected: FAIL (`score` module does not exist)

- [ ] **Step 7: Implement score.rs**

Create `zmod/llm-compress/src/score.rs`:

```rust
//! Single-line scoring: content features + query keyword weighting (spec §4.4/S1). Purely static, no ML.
//! Used by Search/Log to decide which lines to keep.

const ERROR_KEYWORDS: &[&str] = &["error", "fail", "panic", "exception", "traceback", "fatal"];
const WARN_KEYWORDS: &[&str] = &["warn", "warning"];

/// Line score: error keyword +1.0; warning +0.5; each query term hit +0.3; blank line / pure symbols 0.
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

Add `pub mod score;` in `lib.rs`.

- [ ] **Step 8: Run the score test and pass**

Run: `cd codex-rs && cargo test -p codez-llm-compress --test score_test`
Expected: PASS (4 tests)

- [ ] **Step 9: Full build + clippy + commit**

Run: `cd codex-rs && cargo test -p codez-llm-compress && cargo clippy -p codez-llm-compress --all-targets`
Expected: all green, no warnings

```bash
git add zmod/llm-compress/src/query.rs zmod/llm-compress/src/score.rs \
  zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/query_test.rs zmod/llm-compress/tests/score_test.rs
git commit -m "feat(llm-compress-v2): Task02 shared primitives query.rs + score.rs"
```
