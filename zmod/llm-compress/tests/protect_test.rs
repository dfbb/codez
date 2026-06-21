use codez_llm_compress::config::Config;
use codez_llm_compress::protect::should_protect;

#[test]
fn small_error_output_is_protected() {
    let cfg = Config::disabled(); // protect.error_max_bytes 默认 8192
    let text = "Traceback (most recent call last):\n  File x\nValueError: boom";
    assert!(should_protect(text, None, &cfg));
}

#[test]
fn large_error_output_not_protected() {
    let cfg = Config::disabled();
    let big = format!("error: x\n{}", "padding line\n".repeat(2000)); // > 8192 bytes
    assert!(!should_protect(&big, None, &cfg));
}

#[test]
fn non_error_output_not_protected() {
    let cfg = Config::disabled();
    assert!(!should_protect("just normal output\nline two", None, &cfg));
}

#[test]
fn zero_threshold_disables_protection() {
    let mut cfg = Config::disabled();
    cfg.protect.error_max_bytes = 0;
    assert!(!should_protect("error: boom", None, &cfg));
}
