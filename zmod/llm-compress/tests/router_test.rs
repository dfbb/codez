use codez_llm_compress::config::Config;
use codez_llm_compress::command::CommandHint;
use codez_llm_compress::router::{Budget, CompressOutcome, Compressor, ContentRouter, ContentKind};

/// A fake compressor that claims everything and replaces text with a fixed short string.
struct HalfCompressor;
impl Compressor for HalfCompressor {
    fn name(&self) -> &'static str { "half" }
    fn detect(&self, _t: &str, _b: &Budget) -> bool { true }
    fn compress(&self, text: &str, _b: &Budget) -> CompressOutcome {
        let new = format!("[half]{}", &text[..text.len() / 2]);
        let saved = text.len().saturating_sub(new.len());
        if saved > 0 {
            CompressOutcome::Compressed { text: new, saved_bytes: saved, lossy: true, kind: ContentKind::Text }
        } else {
            CompressOutcome::Unchanged
        }
    }
}

/// Never claims anything.
struct NeverCompressor;
impl Compressor for NeverCompressor {
    fn name(&self) -> &'static str { "never" }
    fn detect(&self, _t: &str, _b: &Budget) -> bool { false }
    fn compress(&self, _t: &str, _b: &Budget) -> CompressOutcome { CompressOutcome::Unchanged }
}

/// detect is hit but compress panics — verify fail-open.
struct PanicCompressor;
impl Compressor for PanicCompressor {
    fn name(&self) -> &'static str { "panic" }
    fn detect(&self, _t: &str, _b: &Budget) -> bool { true }
    fn compress(&self, _t: &str, _b: &Budget) -> CompressOutcome { panic!("boom") }
}

fn budget(cfg: &Config) -> Budget<'_> { Budget { cfg, cmd: None } }

#[test]
fn first_detecting_compressor_wins() {
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(NeverCompressor), Box::new(HalfCompressor)]);
    let input = "0123456789ABCDEF"; // 16 bytes
    let out = r.compress_text(input, &budget(&cfg));
    assert!(out.is_some());
    let (text, _lossy, _kind) = out.unwrap();
    assert!(text.starts_with("[half]"));
}

#[test]
fn no_detect_returns_none() {
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(NeverCompressor)]);
    assert!(r.compress_text("anything", &budget(&cfg)).is_none());
}

#[test]
fn panic_in_compress_is_caught_and_returns_none() {
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(PanicCompressor)]);
    // Must not panic; return None to let the caller preserve the original text.
    let out = r.compress_text("some payload text", &budget(&cfg));
    assert!(out.is_none());
}

#[test]
fn unchanged_outcome_returns_none() {
    struct Claims;
    impl Compressor for Claims {
        fn name(&self) -> &'static str { "claims" }
        fn detect(&self, _t: &str, _b: &Budget) -> bool { true }
        fn compress(&self, _t: &str, _b: &Budget) -> CompressOutcome { CompressOutcome::Unchanged }
    }
    let cfg = Config::disabled();
    let r = ContentRouter::new(vec![Box::new(Claims)]);
    assert!(r.compress_text("xxxx", &budget(&cfg)).is_none());
}

/// Command-aware reordering: "git diff" command hint moves the "diff" compressor to the front for priority output.
#[test]
fn cmd_hint_reorders_diff_to_front() {
    // "diff" compressor: claims everything, tags the text with [diff].
    struct DiffFake;
    impl Compressor for DiffFake {
        fn name(&self) -> &'static str { "diff" }
        fn detect(&self, _t: &str, _b: &Budget) -> bool { true }
        fn compress(&self, text: &str, _b: &Budget) -> CompressOutcome {
            let new = format!("[diff]{}", &text[..1]);
            let saved = text.len().saturating_sub(new.len());
            if saved > 0 {
                CompressOutcome::Compressed { text: new, saved_bytes: saved, lossy: true, kind: ContentKind::Text }
            } else {
                CompressOutcome::Unchanged
            }
        }
    }
    // "search" compressor: also claims everything, tags the text with [search].
    struct SearchFake;
    impl Compressor for SearchFake {
        fn name(&self) -> &'static str { "search" }
        fn detect(&self, _t: &str, _b: &Budget) -> bool { true }
        fn compress(&self, text: &str, _b: &Budget) -> CompressOutcome {
            let new = format!("[search]{}", &text[..1]);
            let saved = text.len().saturating_sub(new.len());
            if saved > 0 {
                CompressOutcome::Compressed { text: new, saved_bytes: saved, lossy: true, kind: ContentKind::Text }
            } else {
                CompressOutcome::Unchanged
            }
        }
    }

    let cfg = Config::disabled();
    // Without command hint: search comes first, should get [search] output.
    let r = ContentRouter::new(vec![Box::new(SearchFake), Box::new(DiffFake)]);
    let text = "0123456789ABCDEF"; // 16 bytes, compressible
    let (out_no_cmd, _, _) = r.compress_text(text, &budget(&cfg)).unwrap();
    assert!(out_no_cmd.starts_with("[search]"), "without command hint, search should be first");

    // With "git diff" command hint: diff is reordered to the front, should get [diff] output.
    let cmd = CommandHint { program: "git".into(), argv: vec!["diff".into()] };
    let b_with_cmd = Budget { cfg: &cfg, cmd: Some(&cmd) };
    let r2 = ContentRouter::new(vec![Box::new(SearchFake), Box::new(DiffFake)]);
    let (out_with_cmd, _, _) = r2.compress_text(text, &b_with_cmd).unwrap();
    assert!(out_with_cmd.starts_with("[diff]"), "git diff command hint should reorder diff to the front");
}
