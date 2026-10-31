use ncli::commands::review::Verdict;

const VERDICT: &str = r#"{"findings": [
    {"severity": "low", "title": "Hard-coded tool name", "file": "src/reviewers.rs", "line": 23,
     "explanation": "It only exists in one environment.", "fix": "Move it to configuration."},
    {"severity": "high", "title": "Timeout ignored", "file": "src/process/unix.rs", "line": 201,
     "explanation": " A noisy child never times out. ", "fix": "Check the deadline every loop."}
]}"#;

#[test]
fn findings_render_most_severe_first_as_markdown() {
    let verdict = Verdict::parse(VERDICT).unwrap();
    assert_eq!(
        verdict.to_string(),
        "## ncli review: 2 findings\n\
         \n\
         ### 🔴 High: Timeout ignored\n\
         `src/process/unix.rs:201`\n\
         A noisy child never times out.\n\
         **Fix:** Check the deadline every loop.\n\
         \n\
         ### 🟡 Low: Hard-coded tool name\n\
         `src/reviewers.rs:23`\n\
         It only exists in one environment.\n\
         **Fix:** Move it to configuration."
    );
}

#[test]
fn counts_are_worded_for_people() {
    let none = Verdict::parse(r#"{"findings": []}"#).unwrap();
    assert_eq!(none.to_string(), "## ncli review: no findings");
    let single = r#"{"findings": [{"severity": "medium", "title": "t", "file": "f", "line": 1,
        "explanation": "e", "fix": "x"}]}"#;
    assert!(
        Verdict::parse(single)
            .unwrap()
            .to_string()
            .starts_with("## ncli review: 1 finding\n\n### 🟠 Medium: t")
    );
}

#[test]
fn verdicts_that_do_not_match_the_schema_are_rejected() {
    for invalid in [
        "not json",
        r#"{"findings": [{"severity": "critical", "title": "t", "file": "f", "line": 1,
            "explanation": "e", "fix": "x"}]}"#,
        r#"{"findings": [{"severity": "low", "title": "t", "file": "f", "line": 1,
            "explanation": "e"}]}"#,
        r#"{"findings": [], "summary": "unexpected"}"#,
    ] {
        assert!(Verdict::parse(invalid).is_err(), "{invalid}");
    }
}
