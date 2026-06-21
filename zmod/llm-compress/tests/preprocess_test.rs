use codez_llm_compress::config::PreprocessCfg;
use codez_llm_compress::preprocess::run;

fn cfg() -> PreprocessCfg {
    PreprocessCfg::default()
}

#[test]
fn strip_progress_removes_download_lines_and_marks_lossy() {
    let input = "Downloading foo\nreal line\nDownloading bar\nanother";
    let (out, lossy) = run(input, &cfg());
    assert!(!out.contains("Downloading"));
    assert!(out.contains("real line"));
    assert!(lossy, "删进度条 → lossy=true");
}

#[test]
fn collapse_blank_is_not_lossy() {
    let input = "a\n\n\n\nb";
    let (out, lossy) = run(input, &cfg());
    // 连续空行归一为一个
    assert_eq!(out, "a\n\nb");
    assert!(!lossy, "空行归一是格式重构 → lossy=false");
}

#[test]
fn blob_fold_replaces_long_base64_and_marks_lossy() {
    let blob = "A".repeat(400); // > blob_min_bytes 256
    let input = format!("prefix\n{blob}\nsuffix");
    let (out, lossy) = run(&input, &cfg());
    assert!(!out.contains(&blob));
    assert!(out.contains("[llm-compress: base64"));
    assert!(lossy);
}

#[test]
fn truncate_line_bytes_marks_lossy_utf8_safe() {
    let mut c = cfg();
    c.truncate_line_bytes = 10;
    let input = "中文字符串很长很长很长很长".to_string(); // 多字节
    let (out, lossy) = run(&input, &c);
    assert!(lossy);
    // 产物仍是合法 UTF-8(能正常作为 String 存在即合法)
    assert!(out.len() <= input.len());
}

#[test]
fn dedup_consecutive_not_lossy_and_skips_marker_lines() {
    let input = "x\nx\nx\n[llm-compress: 已有占位]\n[llm-compress: 已有占位]";
    let (out, lossy) = run(input, &cfg());
    assert!(!lossy, "连续重复折叠是格式重构");
    assert!(out.contains("[llm-compress: 上一行 ×3]"));
    // 原文已含 [llm-compress: 前缀的行不参与折叠,原样保留两行
    assert_eq!(out.matches("[llm-compress: 已有占位]").count(), 2);
}

#[test]
fn all_disabled_returns_unchanged() {
    let c = PreprocessCfg { strip_progress: false, collapse_blank: false, truncate_line_bytes: 0, dedup_consecutive: false, blob_min_bytes: 256 };
    let input = "Downloading x\n\n\ny";
    let (out, lossy) = run(input, &c);
    assert_eq!(out, input);
    assert!(!lossy);
}
