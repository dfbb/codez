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
    assert!((0.5..1.0).contains(&s));
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
