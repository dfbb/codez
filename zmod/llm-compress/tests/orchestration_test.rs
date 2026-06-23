//! End-to-end transform: construct requests with real codex types and verify the orchestration chain.
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

/// RAII guard: restore the HOME environment variable to its original value on drop.
struct HomeGuard {
    _dir: tempfile::TempDir,
    original: Option<std::ffi::OsString>,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: tests run serially (#[serial]), so no race condition exists during restoration.
        unsafe {
            match &self.original {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}

/// Write config-zmod.toml to a tempdir and set HOME to that directory;
/// return HomeGuard, which automatically cleans up the tempdir and restores the original HOME on drop.
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
    // SAFETY: tests run serially (#[serial]), so modifying HOME does not race with other tests.
    unsafe { std::env::set_var("HOME", dir.path()) };
    HomeGuard { _dir: dir, original }
}

/// Set HOME to a tempdir without config-zmod.toml → enabled=false.
fn setup_disabled_home() -> HomeGuard {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let original = std::env::var_os("HOME");
    // SAFETY: tests run serially (#[serial]).
    unsafe { std::env::set_var("HOME", dir.path()) };
    HomeGuard { _dir: dir, original }
}

// ---------------------------------------------------------------------------
// Existing tests: unchanged, only adding #[serial] to prevent race with enabled tests on HOME.
// ---------------------------------------------------------------------------

#[test]
#[serial]
fn disabled_config_leaves_request_untouched() {
    let _home = setup_disabled_home();
    // HOME lacks config-zmod.toml → enabled=false → unchanged byte-for-byte
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
// New: end-to-end tests with enabled=true.
// ---------------------------------------------------------------------------

/// Helper: extract text from ResponseItem.
fn get_text(item: &ResponseItem) -> &str {
    match item {
        ResponseItem::FunctionCallOutput { output, .. } => match &output.body {
            FunctionCallOutputBody::Text(s) => s,
            _ => panic!("unexpected body variant"),
        },
        _ => panic!("unexpected item variant"),
    }
}

/// End-to-end iron law test: a JSON product never carries the CCR marker ("[Original: ").
///
/// Construct an input that triggers both dimensions:
///   • Leading progress line "Downloading ..."  → strip_progress removes it  → pre_lossy=true
///   • Remaining content is uniform CSV table       → Tabular compression claims it  → kind=Json, comp_lossy=false
///
/// Buggy behavior: pre_lossy=true triggers attach → JSON followed by "[Original: /path]" → invalid JSON.
/// Fixed behavior: kind=Json → skip attach → product is pure JSON or falls back to original, never JSON+CCR.
///
/// Reachability note:
///   Tabular's detect requires "≥3 rows, ≥2 columns, uniform column count across rows". This test preserves
///   200 rows of uniform CSV after strip_progress, sufficient for Tabular to claim it. However, if Tabular's
///   output is not smaller than the original (size threshold filters it out), the candidate is discarded and
///   the original is retained — in which case the assertion "does not contain JSON+CCR" still holds.
///   Thus regardless of whether compression actually succeeds, the iron law assertion is always satisfied.
#[test]
#[serial]
fn json_kind_never_gets_ccr_attached() {
    let _home = setup_enabled_home();

    // Progress line (removed by strip_progress, triggers pre_lossy=true)
    let progress = "Downloading crates.io index\nFetching metadata 123/456\n";
    // Uniform CSV: 200 rows × 4 columns, claimable by Tabular
    let mut csv = String::from("id,name,score,flag\n");
    for i in 0..200_usize {
        csv.push_str(&format!("{i},item_{i},{},{}\n", i % 100, i % 2));
    }
    // per_item_min_bytes defaults to 1024; ensure input exceeds this threshold
    let input_text = format!("{progress}{csv}");
    assert!(
        input_text.len() >= 1024,
        "input must exceed per_item_min_bytes(1024), actual {} bytes",
        input_text.len()
    );

    let mut r = req(vec![fco("call-json", &input_text)]);
    transform(&mut r, &provider(), "qid-json-ccr");

    let out = get_text(&r.input[0]);

    // Iron law: product must either not contain JSON structure, or not contain the CCR marker — both cannot appear together.
    // Specifically: if output starts with '{' or '[' (JSON product signature), it must not contain "[llm-compress: Original ".
    let looks_like_json = out.trim_start().starts_with('{') || out.trim_start().starts_with('[');
    if looks_like_json {
        assert!(
            !out.contains("[llm-compress: Original "),
            "JSON product must not carry CCR marker!\nFirst 200 bytes of output: {:?}",
            &out[..out.len().min(200)]
        );
        // Additional: JSON product must be parseable.
        serde_json::from_str::<serde_json::Value>(out).unwrap_or_else(|e| {
            panic!(
                "JSON product parse failed (invalid JSON): {e}\nFirst 300 bytes of output: {:?}",
                &out[..out.len().min(300)]
            )
        });
    }

    // Size unchanged or reduced.
    assert!(
        out.len() <= input_text.len(),
        "output ({}) larger than input ({}) bytes",
        out.len(),
        input_text.len()
    );
}

/// End-to-end positive test: large plaintext log is compressed to smaller size after passing through enabled pipeline.
///
/// Use log input with large repetitive patterns — sufficient to be claimed by Log or Truncate compressor.
/// Assert output < input (non-empty compression), and: if output contains CCR marker, the corresponding cache file exists.
#[test]
#[serial]
fn enabled_compresses_large_plaintext() {
    let _home = setup_enabled_home();

    // Construct a >10KB repetitive log (claimable by LogCompressor / TruncateCompressor)
    let line = "[2024-01-01 00:00:00] INFO  some_module: processing request id=12345 status=ok\n";
    let log = line.repeat(200);
    assert!(log.len() > 10_000, "log must be >10KB, actual {} bytes", log.len());

    let mut r = req(vec![fco("call-log", &log)]);
    transform(&mut r, &provider(), "qid-log");

    let out = get_text(&r.input[0]);

    // Main assertion: size reduced.
    assert!(
        out.len() < log.len(),
        "expected compressed size < original, but out={} input={} bytes",
        out.len(),
        log.len()
    );

    // If CCR marker is present, the file referenced by its path must exist.
    if let Some(start) = out.find("[llm-compress: Original ") {
        let rest = &out[start + "[llm-compress: Original ".len()..];
        let path_end = rest.find(']').expect("CCR marker missing closing bracket");
        let path = std::path::Path::new(&rest[..path_end]);
        assert!(
            path.exists(),
            "file referenced by CCR path does not exist: {path:?}"
        );
    }
}

/// End-to-end iron law: a TOON product is written back as BARE TOON, never
/// decorated with a "[llm-compress: 原文 …]" pointer — even when preprocessing
/// was lossy (a leading progress line is stripped → pre_lossy=true).
#[test]
#[serial]
fn toon_kind_never_gets_ccr_attached() {
    let _home = setup_enabled_home();

    // A leading progress line is removed by strip_progress → pre_lossy=true.
    // The remaining single-line JSON array is parsed by JsonCompressor and
    // encoded to TOON. Keep the JSON line < 2000 bytes (preprocess
    // truncate_line_bytes) and the total >= 1024 bytes (per_item_min_bytes).
    let mut objs = String::from("[");
    for i in 0..40u32 {
        if i > 0 {
            objs.push(',');
        }
        objs.push_str(&format!(r#"{{"id":{i},"name":"item_{i}"}}"#));
    }
    objs.push(']');
    assert!(objs.len() < 2000 && objs.len() > 600, "json line len = {}", objs.len());
    let input_text = format!("Downloading crates.io index\n{objs}");
    assert!(input_text.len() >= 1024, "input must exceed per_item_min_bytes: {}", input_text.len());

    let mut r = req(vec![fco("call-toon", &input_text)]);
    transform(&mut r, &provider(), "qid-toon-ccr");

    let out = get_text(&r.input[0]);

    // The output must NOT carry a CCR pointer.
    assert!(
        !out.contains("[llm-compress: 原文 "),
        "TOON product must not carry a CCR pointer!\nout: {:?}",
        &out[..out.len().min(300)]
    );
    // And it must have actually been compressed (smaller than input) and be
    // valid TOON that round-trips. (If for some reason it wasn't claimed, the
    // output would equal pre-processed text; assert the compression happened.)
    assert!(out.len() < input_text.len(), "expected compression: {} vs {}", out.len(), input_text.len());
    let back: serde_json::Value = toon_format::decode_default(out)
        .expect("output must be decodable TOON");
    assert!(back.is_array(), "decoded TOON should be the original array");
}
