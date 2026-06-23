use codez_llm_compress::score::line_score;

#[test]
fn error_lines_score_high() {
    assert!(line_score("ERROR: something panicked") >= 1.0);
    assert!(line_score("thread panicked at foo.rs:42") >= 1.0);
    assert!(line_score("  Traceback (most recent call last):") >= 1.0);
}

#[test]
fn warning_lines_score_medium() {
    let s = line_score("warning: unused variable x");
    assert!((0.5..1.0).contains(&s));
}

#[test]
fn plain_lines_score_low() {
    assert!(line_score("just a normal line of output") < 0.5);
    assert_eq!(line_score(""), 0.0);
    assert_eq!(line_score("   "), 0.0);
}
