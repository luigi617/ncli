use ncli::commands::code::review::{Report, Verdict};

fn render(verdict: &Verdict) -> String {
    Report {
        prog: "ncli code review",
        verdict,
    }
    .to_string()
}

const VERDICT: &str = r#"{"change_accessed": true, "findings": [
    {"severity": "low", "title": "Hard-coded tool name", "file": "src/reviewers.rs", "line": 23,
     "explanation": "It only exists in one environment.", "fix": "Move it to configuration."},
    {"severity": "high", "title": "Timeout ignored", "file": "src/process/unix.rs", "line": 201,
     "explanation": " A noisy child never times out. ", "fix": "Check the deadline every loop."}
]}"#;

#[test]
fn findings_render_most_severe_first_as_markdown() {
    let verdict = Verdict::parse(VERDICT).unwrap();
    assert_eq!(
        render(&verdict),
        "## ncli code review: 2 findings\n\
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
    let none = Verdict::parse(r#"{"change_accessed": true, "findings": []}"#).unwrap();
    assert_eq!(render(&none), "## ncli code review: no findings");
    let single = r#"{"change_accessed": true, "findings": [{"severity": "medium", "title": "t", "file": "f", "line": 1,
        "explanation": "e", "fix": "x"}]}"#;
    assert!(
        render(&Verdict::parse(single).unwrap())
            .starts_with("## ncli code review: 1 finding\n\n### 🟠 Medium: t")
    );
}

#[test]
fn verdicts_that_do_not_match_the_schema_are_rejected() {
    let finding = |line: &str| {
        format!(
            r#"{{"change_accessed": true, "findings": [{{"severity": "low", "title": "t",
                "file": "f", "line": {line}, "explanation": "e", "fix": "x"}}]}}"#
        )
    };
    for invalid in [
        "not json".to_owned(),
        finding("0"),
        finding("-1"),
        finding("4294967296"),
        r#"{"findings": []}"#.to_owned(),
        finding("1").replace("\"low\"", "\"critical\""),
        finding("1").replace(", \"fix\": \"x\"", ""),
        r#"{"change_accessed": true, "findings": [], "summary": "unexpected"}"#.to_owned(),
    ] {
        assert!(Verdict::parse(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn model_text_cannot_break_the_markdown_structure() {
    let verdict = Verdict::parse(
        r#"{"change_accessed": true, "findings": [{"severity": "low",
            "title": "Broken\n# heading", "file": "src/`odd`\nname.rs", "line": 2,
            "explanation": "e\n\n### 🔴 High: fake", "fix": "x\ny"}]}"#,
    )
    .unwrap();
    assert_eq!(
        render(&verdict),
        "## ncli code review: 1 finding\n\n### 🟡 Low: Broken # heading\n``src/`odd` name.rs:2``\ne ### 🔴 High: fake\n**Fix:** x y"
    );
}

#[test]
fn a_location_starting_with_a_backtick_is_padded() {
    let verdict = Verdict::parse(
        r#"{"change_accessed": true, "findings": [{"severity": "low", "title": "t",
            "file": "`f", "line": 1, "explanation": "e", "fix": "x"}]}"#,
    )
    .unwrap();
    assert!(render(&verdict).contains("\n`` `f:1 ``\n"));
}
