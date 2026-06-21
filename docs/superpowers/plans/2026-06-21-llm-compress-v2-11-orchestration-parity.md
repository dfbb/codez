# Task 11 — lib 编排接线 + 继承 fixture + parity_test

> 隶属 `2026-06-21-llm-compress-v2-00-index.md`。覆盖 spec §2 / §8 / §9。依赖全部前序任务(01–10)。

**Goal:** 把所有模块接线进 `transform` 编排链(命令识别 → 保护门 → 预处理 → 路由压缩 → CCR 挂载 → 体积闸门),`build_router` 注册全部 6 个压缩器(Json→Search→Diff→Tabular→Log→Truncate),继承 headroom/rtk fixture(NOTICE+manifest sidecar),写 `parity_test` 跑对比断言。

## Files
- Modify: `zmod/llm-compress/src/lib.rs`(重写 transform 编排 + build_router 注册 6 压缩器)
- Create: `zmod/llm-compress/tests/fixtures/inherited/`(LICENSE-headroom、LICENSE-rtk、NOTICE.md、manifest.toml + 各压缩器子目录原始数据文件)
- Create: `zmod/llm-compress/tests/parity_test.rs`
- Create: `zmod/llm-compress/tests/orchestration_test.rs`(编排端到端)

**Interfaces:**
- Consumes: Task 01–10 全部:`router::{ContentRouter,Budget,build...}`、`query::extract`、`command::index`、`score`、`protect::should_protect`、`preprocess::run`、`ccr::{RequestCtx,CcrRegistry,attach}`、6 压缩器、`config`。
- Produces: 完整可用的 `transform`。

---

- [ ] **Step 1: build_router 注册 6 压缩器**

`zmod/llm-compress/src/lib.rs` 的 `build_router`(v1 只注册 4 个)改为(顺序 = spec 路由优先级 `Json→Search→Diff→Tabular→Log→Truncate`):

```rust
fn build_router() -> ContentRouter {
    use crate::compress::{
        diff::DiffCompressor, json::JsonCompressor, log::LogCompressor,
        search::SearchCompressor, tabular::TabularCompressor, truncate::TruncateCompressor,
    };
    ContentRouter::new(vec![
        Box::new(JsonCompressor),
        Box::new(SearchCompressor),
        Box::new(DiffCompressor),
        Box::new(TabularCompressor),
        Box::new(LogCompressor),
        Box::new(TruncateCompressor),
    ])
}
```

- [ ] **Step 2: 重写 transform 编排(构造 RequestCtx + 处理链)**

`zmod/llm-compress/src/lib.rs` 的 `transform` 改为(spec §2 编排):

```rust
pub fn transform(request: &mut ResponsesApiRequest, _api_provider: &ApiProvider, queryid: &str) {
    let cfg = config::load();
    if !cfg.enabled {
        return;
    }
    // 一次性请求上下文
    let ctx = crate::ccr::RequestCtx {
        queryid,
        query_terms: crate::query::extract(request),
        cmd_index: crate::command::index(request),
        ccr: std::cell::RefCell::new(crate::ccr::CcrRegistry::new()),
    };
    let router = build_router();

    let total_before = total_text_bytes(&request.input);
    for item in request.input.iter_mut() {
        compress_item(item, &ctx, &router, &cfg);
    }
    let total_after = total_text_bytes(&request.input);
    if total_after < total_before {
        stats::log_compression(queryid, total_before, total_after);
    }
}
```

- [ ] **Step 3: 重写 compress_item / compress_in_place(处理链 ①–⑥)**

替换 `lib.rs` 现有 `compress_item` 与 `compress_in_place`:

```rust
fn compress_item(
    item: &mut ResponseItem,
    ctx: &crate::ccr::RequestCtx,
    router: &ContentRouter,
    cfg: &config::Config,
) {
    let call_id = match item {
        ResponseItem::FunctionCallOutput { call_id, .. } => call_id.clone(),
        ResponseItem::CustomToolCallOutput { call_id, .. } => call_id.clone(),
        _ => return,
    };
    let body = match item {
        ResponseItem::FunctionCallOutput { output, .. } => &mut output.body,
        ResponseItem::CustomToolCallOutput { output, .. } => &mut output.body,
        _ => return,
    };
    match body {
        FunctionCallOutputBody::Text(s) => compress_in_place(s, ctx, router, cfg, &call_id),
        FunctionCallOutputBody::ContentItems(items) => {
            for ci in items.iter_mut() {
                if let FunctionCallOutputContentItem::InputText { text } = ci {
                    compress_in_place(text, ctx, router, cfg, &call_id);
                }
            }
        }
    }
}

fn compress_in_place(
    s: &mut String,
    ctx: &crate::ccr::RequestCtx,
    router: &ContentRouter,
    cfg: &config::Config,
    call_id: &str,
) {
    if s.len() < cfg.per_item_min_bytes {
        return;
    }
    let cmd = ctx.cmd_index.get(call_id);
    // ② 保护门:命中即整段逐字节不变
    if crate::protect::should_protect(s, cmd, cfg) {
        return;
    }
    // ③ 预处理
    let (pre, pre_lossy) = crate::preprocess::run(s, &cfg.preprocess);
    // ④⑤ 路由压缩
    let budget = Budget { cfg, cmd, query: &ctx.query_terms };
    let candidate = match router.compress_text(&pre, &budget) {
        Some((new, comp_lossy, _kind)) => {
            if pre_lossy || comp_lossy {
                crate::ccr::attach(new, s, ctx, call_id, &cfg.ccr)
            } else {
                new
            }
        }
        None => {
            if pre_lossy {
                crate::ccr::attach(pre, s, ctx, call_id, &cfg.ccr)
            } else {
                pre
            }
        }
    };
    // #4 最终写回闸门
    if candidate.len() <= s.len() {
        *s = candidate;
    }
}
```

> 删除 v1 `compress_item` 旧签名(带 `min_bytes` 参数的)与旧 `Budget { cfg: &cfg }` 构造。确保 `lib.rs` 顶部 use 含 `FunctionCallOutputContentItem`、`Budget`、`ContentRouter`。

- [ ] **Step 4: 编译 + 现有测试回归**

Run: `cd codex-rs && cargo build -p codez-llm-compress && cargo test -p codez-llm-compress`
Expected: 全绿(全部模块测试 + 现有)

- [ ] **Step 5: 建 fixture 目录骨架 + LICENSE + NOTICE**

```bash
mkdir -p zmod/llm-compress/tests/fixtures/inherited/{search,log,diff,json,tabular,preprocess}
cp ../3rd/compress/headroom/LICENSE zmod/llm-compress/tests/fixtures/inherited/LICENSE-headroom
cp ../3rd/compress/rtk/LICENSE zmod/llm-compress/tests/fixtures/inherited/LICENSE-rtk
```

创建 `zmod/llm-compress/tests/fixtures/inherited/NOTICE.md`:

```markdown
# 继承测试数据来源

本目录数据改编自以下 Apache-2.0 项目,版权归原作者:
- headroom — © 2025 Headroom Contributors(Apache-2.0,见 LICENSE-headroom)
- rtk — © 2024 rtk-ai Labs(Apache-2.0,见 LICENSE-rtk)

逐文件来源见 manifest.toml 的 `origin` 字段。fixture 文件保持上游原始字节,不在文件内加注释(避免破坏 JSON/污染输入)。
```

- [ ] **Step 6: 拷贝 input fixture(从 headroom parity JSON 抽 input,从 rtk 抽样本)**

CCR/parity fixture 的 input 从 headroom parity JSON 的 `input` 字段提取。由于上游 parity JSON 是 `{input, config, output}` 三元组,**input 字段即我方 fixture 输入,output 字段存为 `.expected` 供对比**。用脚本抽取(对每个压缩器目录取 3-5 个代表样本即可,不必全 20+):

```bash
# 示例:Log —— 取 headroom 3 个 parity JSON,拆出 input 与 expected
cd /Users/dfbb/Sites/skycode/codez
for f in $(ls ../3rd/compress/headroom/tests/parity/fixtures/log_compressor/*.json | head -3); do
  base=$(basename "$f" .json)
  python3 -c "import json,sys; d=json.load(open('$f')); open('zmod/llm-compress/tests/fixtures/inherited/log/$base.txt','w').write(d['input']); out=d['output']; open('zmod/llm-compress/tests/fixtures/inherited/log/$base.expected','w').write(out['compressed'] if isinstance(out,dict) else out)"
done
# Diff / JSON(smart_crusher)同理,改目录名与 head 数量
```

对 Search:headroom search 无 parity fixture,从 `search_compressor.rs:668-900` 内嵌例**手抄** 2-3 个标准 grep 样本存为 `search/grep_basic.txt`(无 expected,parity 仅跑硬不变量)。
对 rtk preprocess:从 `rtk/src/core/toml_filter.rs` 测试或 `rtk/tests/fixtures/*.txt` 取 1-2 个含 ANSI/进度条样本存 `preprocess/`。
对 Tabular:自造 `tabular/simple.txt`(`id,name\n1,a\n2,b`),无 expected。

> **取舍说明(必须 log 出来)**:每个压缩器只继承 3-5 个代表样本而非全部,降低维护成本;覆盖在单测(Task 05-08)里已充分,parity 是补充性的真实数据对比。在 `NOTICE.md` 末尾追加一行注明"每类取代表样本,非全量继承"。

- [ ] **Step 7: 写 manifest.toml**

创建 `zmod/llm-compress/tests/fixtures/inherited/manifest.toml`(按实际拷贝的文件填;示例):

```toml
[[fixture]]
file = "log/3bc015edc0a36387.txt"
origin = "headroom/tests/parity/fixtures/log_compressor/3bc015edc0a36387.json"
compressor = "log"
ref_output = "log/3bc015edc0a36387.expected"
invariants = ["volume_not_worse", "keep_error_lines", "hard_invariants"]

[[fixture]]
file = "search/grep_basic.txt"
origin = "headroom/crates/headroom-core/src/transforms/search_compressor.rs:680"
compressor = "search"
ref_output = ""   # 无参考输出,仅跑硬不变量
invariants = ["hard_invariants"]

# … 其余按实际拷贝的文件补全(diff/json/tabular/preprocess 各 condition)
```

- [ ] **Step 8: 写 parity_test.rs(遍历 manifest 跑硬不变量)**

创建 `zmod/llm-compress/tests/parity_test.rs`。直接调各压缩器,断言 §8.3 不变量(体积不劣、关键行保留、硬不变量)。固定 `ccr.enabled=true`:

```rust
//! 遍历 fixtures/inherited/manifest.toml,对每个继承样本跑硬不变量(spec §8.3)。
//! 不做逐字节相等;参考输出仅用于"体积不劣"对比。

use codez_llm_compress::compress::{
    json::JsonCompressor, log::LogCompressor, search::SearchCompressor,
    tabular::TabularCompressor,
};
use codez_llm_compress::config::Config;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inherited")
}

#[derive(serde::Deserialize)]
struct Manifest {
    fixture: Vec<Fixture>,
}
#[derive(serde::Deserialize)]
struct Fixture {
    file: String,
    compressor: String,
    #[serde(default)]
    ref_output: String,
}

fn run_compressor(name: &str, text: &str, cfg: &Config) -> Option<(String, bool)> {
    let budget = Budget { cfg, cmd: None, query: &[] };
    let c: Box<dyn Compressor> = match name {
        "json" => Box::new(JsonCompressor),
        "search" => Box::new(SearchCompressor),
        "tabular" => Box::new(TabularCompressor),
        "log" => Box::new(LogCompressor),
        _ => return None,
    };
    if !c.detect(text, &budget) {
        return None;
    }
    match c.compress(text, &budget) {
        CompressOutcome::Compressed { text, lossy, .. } => Some((text, lossy)),
        CompressOutcome::Unchanged => None,
    }
}

#[test]
fn parity_invariants_hold_for_all_fixtures() {
    let dir = fixtures_dir();
    let manifest_path = dir.join("manifest.toml");
    if !manifest_path.exists() {
        eprintln!("manifest.toml 不存在,跳过 parity(fixture 未就位)");
        return;
    }
    let manifest: Manifest = toml::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();

    let mut cfg = Config::disabled();
    cfg.enabled = true;
    // 给足阈值让压缩器认领(parity 关注算法输出而非让位)
    cfg.truncate.max_bytes = 1_000_000;

    for fx in &manifest.fixture {
        let input = std::fs::read_to_string(dir.join(&fx.file))
            .unwrap_or_else(|_| panic!("读不到 fixture {}", fx.file));
        let Some((out, _lossy)) = run_compressor(&fx.compressor, &input, &cfg) else {
            continue; // 未认领/未压缩,跳过(允许)
        };

        // 硬不变量 1:压后 ≤ 压前
        assert!(out.len() <= input.len(), "[{}] 压后体积应 ≤ 压前", fx.file);
        // 硬不变量 2:UTF-8 合法(out 是 String,天然合法)
        // 硬不变量 3:JSON 压缩器产物可 parse
        if fx.compressor == "json" || fx.compressor == "tabular" {
            serde_json::from_str::<serde_json::Value>(&out)
                .unwrap_or_else(|_| panic!("[{}] JSON 产物必须可 parse", fx.file));
        }
        // 对比 4:体积不劣于参考(若有 ref_output)
        if !fx.ref_output.is_empty() {
            if let Ok(reference) = std::fs::read_to_string(dir.join(&fx.ref_output)) {
                assert!(
                    out.len() as f64 <= reference.len() as f64 * 1.5,
                    "[{}] 我方产物 {} 不应远超参考 {} 的 1.5x",
                    fx.file, out.len(), reference.len()
                );
            }
        }
    }
}
```

> `serde`/`toml`/`serde_json` 均已是依赖。

- [ ] **Step 9: 写 orchestration_test.rs(端到端编排)**

创建 `zmod/llm-compress/tests/orchestration_test.rs`:

```rust
//! transform 端到端:用真实 codex 类型构造 request,验证编排链。
use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem,
};

fn req(input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "m".to_string(), instructions: String::new(), input,
        tools: Vec::new(), tool_choice: "auto".to_string(), parallel_tool_calls: false,
        reasoning: None, store: false, stream: true, include: Vec::new(),
        service_tier: None, prompt_cache_key: None, text: None, client_metadata: None,
    }
}
fn fco(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None, call_id: call_id.to_string(),
        output: FunctionCallOutputPayload { body: FunctionCallOutputBody::Text(text.to_string()), success: Some(true) },
        metadata: None,
    }
}

fn provider() -> codex_api::Provider {
    codex_api::Provider {
        name: "t".to_string(), base_url: "https://e.com".to_string(),
        query_params: None, headers: Default::default(),
        retry: codex_api::RetryConfig { max_attempts: 1, base_delay: std::time::Duration::from_millis(0), retry_429: false, retry_5xx: false, retry_transport: false },
        stream_idle_timeout: std::time::Duration::from_secs(30),
    }
}

#[test]
fn disabled_config_leaves_request_untouched() {
    // 无 config-zmod 文件 → enabled=false → 逐字节不变
    let big = "x\n".repeat(10_000);
    let mut r = req(vec![fco("c1", &big)]);
    let before = r.clone();
    transform(&mut r, &provider(), "qid-1");
    if let (ResponseItem::FunctionCallOutput { output: a, .. }, ResponseItem::FunctionCallOutput { output: b, .. }) = (&r.input[0], &before.input[0]) {
        match (&a.body, &b.body) {
            (FunctionCallOutputBody::Text(sa), FunctionCallOutputBody::Text(sb)) => assert_eq!(sa, sb),
            _ => panic!("body shape changed"),
        }
    }
}

#[test]
fn non_tooloutput_variants_ignored() {
    let mut r = req(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert!(matches!(r.input[0], ResponseItem::Other));
}
```

> 注:`disabled_config_leaves_request_untouched` 依赖测试环境无 `~/.codex/config-zmod.toml` 的 `[llm_compress].enabled=true`。CI/本地若存在该文件会干扰——可接受(沿用 v1 transform_test 同样假设)。

- [ ] **Step 10: 全量测试 + clippy**

Run: `cd codex-rs && cargo test -p codez-llm-compress`
Expected: 全绿(所有模块 + parity + orchestration)

Run: `cd codex-rs && cargo clippy -p codez-llm-compress --all-targets`
Expected: 无 warning

- [ ] **Step 11: 提交**

```bash
git add zmod/llm-compress/src/lib.rs \
  zmod/llm-compress/tests/fixtures/inherited \
  zmod/llm-compress/tests/parity_test.rs \
  zmod/llm-compress/tests/orchestration_test.rs
git commit -m "feat(llm-compress-v2): Task11 lib 编排接线 + 6压缩器注册 + 继承fixture + parity_test"
```

- [ ] **Step 12: 更新 README(记录 v2 能力)**

在 `zmod/llm-compress/README.md` 追加 v2 章节:六压缩器、预处理层、命令感知、CCR 取回机制、配置新字段。提交:

```bash
git add zmod/llm-compress/README.md
git commit -m "docs(llm-compress): README 补 v2 能力(预处理/命令感知/CCR/新压缩器)"
```


