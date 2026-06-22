# Task 4: lib.rs — 两级 route() 与 should_bypass_websocket

> 隶属 [llm-router 实现计划](README.md)。先读 README 的 Global Constraints。

**Files:**
- Modify: `zmod/llm-switch/src/lib.rs`
- Test: 单元测试写在 `lib.rs` 内现有 `#[cfg(test)] mod tests`(追加)

**Interfaces:**
- Consumes:
  - Task 1:`Config.purpose: HashMap<String, String>`。
  - Task 2:`purpose_from_source(&SessionSource) -> Option<Purpose>`、`Purpose::as_key`。
  - Task 3:`request_has_namespace_tools(&ResponsesApiRequest) -> bool`。
- Produces(供 Task 5 patch 调用,签名逐字固定):
  - `pub fn route(provider_id: &str, source: Option<&codex_protocol::protocol::SessionSource>, request: &codex_api::ResponsesApiRequest) -> Option<Route>`
  - `pub fn should_bypass_websocket(provider_id: &str, source: Option<&codex_protocol::protocol::SessionSource>) -> bool`
  - `run(...)`、`Route`、`enabled()` 不变。

---

## 背景(给零上下文工程师)

当前 `route()` 是单级:`route(model_provider_id) -> Option<Route>`,内部 `route_in(cfg, id)` 查 `cfg.providers`。spec §4 要升级成**两级查表 + namespace 预检**,并新增 WS 绕过判定。

⚠️ **签名变更影响 patch**:`route()` 现签名 `route(model_provider_id: &str)` 被 patch 调用。本 task 改签名后,Task 5 的 patch 必须同步(已在 Task 5 写明)。crate 内现有 `#[cfg(test)] mod tests` 直接调 `route_in`,不调 `route`,故不受签名变更影响。

**两级 route 逻辑(spec §4,逐步)**——把决策抽成纯函数 `route_in(cfg, provider_id, purpose, has_ns_tools)` 便于单测(不碰全局配置):

```text
1. !cfg.enabled                              -> None
2.（purpose 已由调用方算好传入）
3. purpose 命中且 cfg.purpose 配了该用途:
     target = cfg.purpose[purpose.as_key()]
     a. cfg.providers 无 target              -> warn,落第 4 步
     b. has_ns_tools == true                 -> warn,落第 4 步
     c. 否则                                  -> Some(Route{target, cfg})
4. cfg.providers 有 provider_id              -> Some(Route{provider_id, cfg})  // 不看 has_ns_tools
5. 否则                                       -> None
```

**should_bypass_websocket(spec §4.2)**:只看 source(此时 request 未构造,拿不到 tools)。`enabled && purpose 命中 && cfg.purpose 配了该用途 && 目标 provider 存在` → true。**不做 namespace 预检**(已知边界:罕见组合会损失一趟 WS,功能无损)。

---

- [ ] **Step 1: 写失败测试 — 两级 route 与 bypass**

在 `zmod/llm-switch/src/lib.rs` 的 `#[cfg(test)] mod tests`(当前 244-258 行)内,在现有两个测试后追加。该模块已有 `use super::*;`(把根模块的 `Purpose` 等带入作用域,**无需**再写 `use crate::purpose::Purpose;`)。先准备一个构造 Config 的 helper 和测试:

```rust
    fn cfg_with_purpose() -> Config {
        // providers: gpt(主)、cheap(用途目标);purpose: compact->cheap, review->nonexist
        load_config_from_str(
            r#"
[llm-switch]
enabled = true
[llm-switch.providers.gpt]
connector = "chat"
auth = "bearer"
[llm-switch.providers.cheap]
connector = "chat"
auth = "bearer"
[llm-switch.purpose]
compact = "cheap"
review  = "nonexist"
"#,
            false,
        )
        .unwrap()
    }

    #[test]
    fn purpose_hit_routes_to_target() {
        let cfg = cfg_with_purpose();
        // compact 命中 -> 目标 cheap,无 ns 工具
        let r = route_in(&cfg, "gpt", Some(Purpose::Compact), false).expect("route some");
        assert_eq!(r.provider_id, "cheap");
    }

    #[test]
    fn purpose_bad_mapping_falls_back_to_provider_id() {
        let cfg = cfg_with_purpose();
        // review -> "nonexist" 不存在 -> 回退 provider-id(gpt 存在)
        let r = route_in(&cfg, "gpt", Some(Purpose::Review), false).expect("route some");
        assert_eq!(r.provider_id, "gpt");
    }

    #[test]
    fn purpose_with_ns_tools_falls_back_to_provider_id() {
        let cfg = cfg_with_purpose();
        // compact 命中但含 ns 工具 -> 放弃用途路由,回退 provider-id
        let r = route_in(&cfg, "gpt", Some(Purpose::Compact), true).expect("route some");
        assert_eq!(r.provider_id, "gpt");
    }

    #[test]
    fn no_purpose_uses_provider_id() {
        let cfg = cfg_with_purpose();
        let r = route_in(&cfg, "gpt", None, false).expect("route some");
        assert_eq!(r.provider_id, "gpt");
    }

    #[test]
    fn no_purpose_unknown_provider_is_none() {
        let cfg = cfg_with_purpose();
        assert!(route_in(&cfg, "unknown", None, false).is_none());
    }

    #[test]
    fn purpose_hit_unknown_provider_id_still_routes_to_purpose() {
        let cfg = cfg_with_purpose();
        // 主 provider 不存在,但 compact 命中 cheap -> 用途路由仍生效
        let r = route_in(&cfg, "unknown-main", Some(Purpose::Compact), false).expect("route some");
        assert_eq!(r.provider_id, "cheap");
    }

    #[test]
    fn disabled_never_routes_two_level() {
        let cfg = load_config_from_str(
            "[llm-switch]\nenabled=false\n[llm-switch.providers.cheap]\nconnector=\"chat\"\nauth=\"bearer\"\n[llm-switch.purpose]\ncompact=\"cheap\"\n",
            false,
        ).unwrap();
        assert!(route_in(&cfg, "gpt", Some(Purpose::Compact), false).is_none());
    }

    #[test]
    fn bypass_ws_true_when_purpose_target_exists() {
        let cfg = cfg_with_purpose();
        assert!(should_bypass_in(&cfg, "gpt", Some(Purpose::Compact)));
    }

    #[test]
    fn bypass_ws_false_when_no_purpose() {
        let cfg = cfg_with_purpose();
        assert!(!should_bypass_in(&cfg, "gpt", None));
    }

    #[test]
    fn bypass_ws_false_on_bad_mapping() {
        let cfg = cfg_with_purpose();
        // review -> nonexist:目标不存在 -> 不绕 WS(会回退 provider-id,本可走原生 WS)
        assert!(!should_bypass_in(&cfg, "gpt", Some(Purpose::Review)));
    }

    #[test]
    fn bypass_ws_false_when_disabled() {
        let cfg = load_config_from_str(
            "[llm-switch]\nenabled=false\n[llm-switch.providers.cheap]\nconnector=\"chat\"\nauth=\"bearer\"\n[llm-switch.purpose]\ncompact=\"cheap\"\n",
            false,
        ).unwrap();
        assert!(!should_bypass_in(&cfg, "gpt", Some(Purpose::Compact)));
    }
```

- [ ] **Step 2: 运行测试,确认失败**

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -20
```

Expected: 编译失败,`cannot find function 'route_in'`(签名不匹配:现有 `route_in` 是 2 参)/ `cannot find function 'should_bypass_in'`。

- [ ] **Step 3: 重写 route_in,新增 should_bypass_in 与公开入口**

编辑 `zmod/llm-switch/src/lib.rs`。先在文件顶部 `use` 区(`use std::sync::OnceLock;` 一带)只引入 `SessionSource`。

```rust
use codex_protocol::protocol::SessionSource;
```

> ⚠️ **不要**再写 `use crate::purpose::{purpose_from_source, Purpose};`——Task 2 已在本模块 `pub use purpose::{purpose_from_source, Purpose};`,它们在 lib.rs 根模块内可直接按裸名使用;再写一条非 glob 同名 `use` 会触发 E0252 duplicate import 编译错误。下面 `route_in`/`route()` 直接引用 `Purpose`、`purpose_from_source`、`request_has_namespace_tools` 即可(均已在 crate 根 in scope)。

把现有 `route_in`(当前 230-236 行)整体替换为两级版本 + bypass 纯函数:

```rust
/// 两级路由纯函数(spec §4):purpose 优先 -> provider-id 回退 -> None。
/// `has_ns_tools` 由调用方用 request 预检算好(spec §4.1)。
fn route_in(
    cfg: &Config,
    provider_id: &str,
    purpose: Option<Purpose>,
    has_ns_tools: bool,
) -> Option<Route> {
    if !cfg.enabled {
        return None;
    }
    // 第 3 步:purpose 分支
    if let Some(p) = purpose {
        if let Some(target) = cfg.purpose.get(p.as_key()) {
            match cfg.providers.get(target) {
                None => {
                    tracing::warn!(
                        "llm-switch purpose '{}' -> unknown provider '{}', 回退 provider-id 路由",
                        p.as_key(),
                        target
                    );
                }
                Some(_) if has_ns_tools => {
                    tracing::warn!(
                        "llm-switch purpose '{}' 命中但请求含不可表达工具,放弃用途路由、回退 provider-id",
                        p.as_key()
                    );
                }
                Some(pc) => {
                    return Some(Route { provider_id: target.clone(), cfg: pc.clone() });
                }
            }
        }
    }
    // 第 4 步:provider-id 分支(不看 has_ns_tools,保留 v1 硬失败契约)
    cfg.providers.get(provider_id).map(|p| Route {
        provider_id: provider_id.to_string(),
        cfg: p.clone(),
    })
}

/// WS 绕过纯函数(spec §4.2):只看 source/purpose,不做 namespace 预检。
fn should_bypass_in(cfg: &Config, _provider_id: &str, purpose: Option<Purpose>) -> bool {
    if !cfg.enabled {
        return false;
    }
    match purpose {
        Some(p) => cfg
            .purpose
            .get(p.as_key())
            .map(|target| cfg.providers.contains_key(target))
            .unwrap_or(false),
        None => false,
    }
}
```

- [ ] **Step 4: 重写公开 route(),新增 should_bypass_websocket()**

把现有 `pub fn route(...)`(当前 238-242 行)替换为新签名 + bypass 入口:

```rust
/// 两级路由入口(Task 5 patch 调用契约,签名逐字固定)。
/// purpose 由 source 解析;namespace 预检对 purpose 分支生效(spec §4 / §4.1)。
pub fn route(
    provider_id: &str,
    source: Option<&SessionSource>,
    request: &codex_api::ResponsesApiRequest,
) -> Option<Route> {
    let cfg = loaded();
    let purpose = source.and_then(purpose_from_source);
    let has_ns_tools = purpose.is_some() && request_has_namespace_tools(request);
    route_in(cfg, provider_id, purpose, has_ns_tools)
}

/// 传输层绕过判定(Task 5 patch 调用契约,签名逐字固定)。
/// purpose 命中且映射目标存在时返回 true,使 stream() 跳过 WebSocket、走 HTTP(spec §4.2)。
pub fn should_bypass_websocket(
    provider_id: &str,
    source: Option<&SessionSource>,
) -> bool {
    let cfg = loaded();
    let purpose = source.and_then(purpose_from_source);
    should_bypass_in(cfg, provider_id, purpose)
}
```

> 优化说明:`route()` 里 `has_ns_tools` 仅在 `purpose.is_some()` 时才调 `request_has_namespace_tools`(短路),避免对原生/provider-id 路径做无谓扫描。

- [ ] **Step 5: 运行测试,确认通过**

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -25
```

Expected: 新增 11 个测试全 PASS;原有 `disabled_never_routes` / `enabled_routes_known_provider`(调 `route_in` 旧 2 参签名)会编译失败——**需同步更新**:把这两个旧测试的 `route_in(&cfg, "x")` 改为 `route_in(&cfg, "x", None, false)`。改完重跑直至全绿。

- [ ] **Step 6: 确认 run_test.rs 等集成测试不受签名变更影响**

```bash
cd codex-rs && cargo test -p codez-llm-switch 2>&1 | tail -10
```

Expected: 全部测试 PASS(集成测试调的是 `run`/`testing::*`,不调 `route`,不受影响)。

- [ ] **Step 7: clippy**

```bash
cd codex-rs && cargo clippy -p codez-llm-switch --all-targets 2>&1 | tail -15
```

Expected: 无 error;无本 task 新增 warning。

- [ ] **Step 8: 提交**

```bash
cd /Users/dfbb/Sites/skycode/codez-llm-router
git add zmod/llm-switch/src/lib.rs
git commit -m "feat(llm-switch): 两级 route() + should_bypass_websocket(purpose 路由核心)"
```
