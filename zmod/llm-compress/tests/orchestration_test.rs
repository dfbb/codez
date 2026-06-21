//! transform 端到端:用真实 codex 类型构造 request,验证编排链。
use codez_llm_compress::transform;
use codex_api::ResponsesApiRequest;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputPayload, ResponseItem,
};
use serial_test::serial;

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
fn fco(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success: Some(true),
        },
        metadata: None,
    }
}

fn provider() -> codex_api::Provider {
    codex_api::Provider {
        name: "t".to_string(),
        base_url: "https://e.com".to_string(),
        query_params: None,
        headers: Default::default(),
        retry: codex_api::RetryConfig {
            max_attempts: 1,
            base_delay: std::time::Duration::from_millis(0),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: std::time::Duration::from_secs(30),
    }
}

/// RAII 守卫:在 drop 时恢复 HOME 环境变量至原值。
struct HomeGuard {
    _dir: tempfile::TempDir,
    original: Option<std::ffi::OsString>,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: 测试串行执行(#[serial]),恢复时不存在竞争。
        unsafe {
            match &self.original {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// 向 tempdir 写 config-zmod.toml 并把 HOME 设为该目录;
/// 返回 HomeGuard,drop 时自动清理 tempdir 并恢复原 HOME。
fn setup_enabled_home() -> HomeGuard {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let codex_dir = dir.path().join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("mkdir .codex");
    std::fs::write(
        codex_dir.join("config-zmod.toml"),
        "[llm_compress]\nenabled = true\n",
    )
    .expect("write config-zmod.toml");
    let original = std::env::var_os("HOME");
    // SAFETY: 测试串行执行(#[serial]),修改 HOME 不会与其他测试竞争。
    unsafe { std::env::set_var("HOME", dir.path()) };
    HomeGuard { _dir: dir, original }
}

/// HOME 设为不含 config-zmod.toml 的 tempdir → enabled=false。
fn setup_disabled_home() -> HomeGuard {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let original = std::env::var_os("HOME");
    // SAFETY: 测试串行执行(#[serial])。
    unsafe { std::env::set_var("HOME", dir.path()) };
    HomeGuard { _dir: dir, original }
}

// ---------------------------------------------------------------------------
// 原有测试:保持不变,仅加 #[serial] 防止与 enabled 测试竞争 HOME。
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn disabled_config_leaves_request_untouched() {
    let _home = setup_disabled_home();
    // HOME 无 config-zmod.toml → enabled=false → 逐字节不变
    let big = "x\n".repeat(10_000);
    let mut r = req(vec![fco("c1", &big)]);
    let before = r.clone();
    transform(&mut r, &provider(), "qid-1");
    if let (
        ResponseItem::FunctionCallOutput { output: a, .. },
        ResponseItem::FunctionCallOutput { output: b, .. },
    ) = (&r.input[0], &before.input[0])
    {
        match (&a.body, &b.body) {
            (FunctionCallOutputBody::Text(sa), FunctionCallOutputBody::Text(sb)) => {
                assert_eq!(sa, sb)
            }
            _ => panic!("body shape changed"),
        }
    }
}

#[test]
#[serial]
fn non_tooloutput_variants_ignored() {
    let _home = setup_disabled_home();
    let mut r = req(vec![ResponseItem::Other]);
    transform(&mut r, &provider(), "qid-2");
    assert!(matches!(r.input[0], ResponseItem::Other));
}

// ---------------------------------------------------------------------------
// 新增:enabled=true 端到端测试。
// ---------------------------------------------------------------------------

/// 辅助:从 ResponseItem 取出文本。
fn get_text(item: &ResponseItem) -> &str {
    match item {
        ResponseItem::FunctionCallOutput { output, .. } => match &output.body {
            FunctionCallOutputBody::Text(s) => s,
            _ => panic!("unexpected body variant"),
        },
        _ => panic!("unexpected item variant"),
    }
}

/// 端到端铁律测试:JSON 产物绝不携带 CCR 标记("[原文:")。
///
/// 构造一个同时触发两个维度的输入:
///   • 前置进度行 "Downloading ..."  → strip_progress 删除 → pre_lossy=true
///   • 剩余内容是均匀 CSV 表格       → Tabular 压缩认领  → kind=Json, comp_lossy=false
///
/// BUG 前行为:pre_lossy=true 触发 attach → JSON 后追加 "[原文: /path]" → 非法 JSON。
/// 修复后行为:kind=Json → 跳过 attach → 产物是纯 JSON 或退回原文,绝无 JSON+CCR。
///
/// 可达性说明:
///   Tabular 的 detect 要求"≥3 行、≥2 列、行间列数一致"。本测试在 strip_progress 后
///   保留了 200 行均匀 CSV,足以被 Tabular 认领。但若 Tabular 产出不比原文小(体积
///   闸门过滤),候选被丢弃、原文保留——此时断言"不含 JSON+CCR"同样成立。
///   因此无论压缩器是否真的压缩成功,铁律断言始终非空。
#[test]
#[serial]
fn json_kind_never_gets_ccr_attached() {
    let _home = setup_enabled_home();

    // 进度行(会被 strip_progress 删除,触发 pre_lossy=true)
    let progress = "Downloading crates.io index\nFetching metadata 123/456\n";
    // 均匀 CSV:200 行 × 4 列,Tabular 可认领
    let mut csv = String::from("id,name,score,flag\n");
    for i in 0..200_usize {
        csv.push_str(&format!("{i},item_{i},{},{}\n", i % 100, i % 2));
    }
    // per_item_min_bytes 默认 1024;确保输入超过该阈值
    let input_text = format!("{progress}{csv}");
    assert!(
        input_text.len() >= 1024,
        "输入需超过 per_item_min_bytes(1024),实际 {} 字节",
        input_text.len()
    );

    let mut r = req(vec![fco("call-json", &input_text)]);
    transform(&mut r, &provider(), "qid-json-ccr");

    let out = get_text(&r.input[0]);

    // 铁律:产物要么不含 JSON 结构,要么不含 CCR 标记——二者不能同时出现。
    // 具体:若输出以 '{' 或 '[' 开头(JSON 产物特征),则不允许含 "[llm-compress: 原文 "。
    let looks_like_json = out.trim_start().starts_with('{') || out.trim_start().starts_with('[');
    if looks_like_json {
        assert!(
            !out.contains("[llm-compress: 原文 "),
            "JSON 产物不得携带 CCR 标记!\n输出前 200 字节: {:?}",
            &out[..out.len().min(200)]
        );
        // 额外:JSON 产物必须可 parse。
        serde_json::from_str::<serde_json::Value>(out).unwrap_or_else(|e| {
            panic!(
                "JSON 产物 parse 失败(非法 JSON): {e}\n输出前 300 字节: {:?}",
                &out[..out.len().min(300)]
            )
        });
    }

    // 体积不变或缩小。
    assert!(
        out.len() <= input_text.len(),
        "输出({})大于输入({})字节",
        out.len(),
        input_text.len()
    );
}

/// 端到端正向测试:大纯文本日志经 enabled 管道压缩后体积缩小。
///
/// 使用含大量重复模式的日志输入——足以被 Log 或 Truncate 压缩器认领。
/// 断言输出 < 输入(非空压缩),以及:若输出含 CCR 标记,则对应缓存文件存在。
#[test]
#[serial]
fn enabled_compresses_large_plaintext() {
    let _home = setup_enabled_home();

    // 构造一个 >10KB 的重复日志(LogCompressor / TruncateCompressor 均可认领)
    let line = "[2024-01-01 00:00:00] INFO  some_module: processing request id=12345 status=ok\n";
    let log = line.repeat(200);
    assert!(log.len() > 10_000, "日志需 >10KB,实际 {} 字节", log.len());

    let mut r = req(vec![fco("call-log", &log)]);
    transform(&mut r, &provider(), "qid-log");

    let out = get_text(&r.input[0]);

    // 主断言:体积缩小。
    assert!(
        out.len() < log.len(),
        "期望压缩后体积 < 原文,但 out={} input={} 字节",
        out.len(),
        log.len()
    );

    // 若含 CCR 标记,路径指向的文件必须存在。
    if let Some(start) = out.find("[llm-compress: 原文 ") {
        let rest = &out[start + "[llm-compress: 原文 ".len()..];
        let path_end = rest.find(']').expect("CCR 标记缺少右括号");
        let path = std::path::Path::new(&rest[..path_end]);
        assert!(
            path.exists(),
            "CCR 路径指向的文件不存在: {path:?}"
        );
    }
}
