use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use ncli::agent::AgentKind;
use ncli::cli::Ncli;
use ncli::commands::Command;
use ncli::commands::code::CodeCommand;
use ncli::commands::code::review::Review;

const LINK: &str = "https://example.com/pr/1";

fn review(argv: &[&str]) -> Review {
    let ncli = Ncli::try_parse_from(["ncli", "code", "review"].iter().chain(argv)).unwrap();
    let Command::Code(CodeCommand::Review(review)) = ncli.command else {
        panic!("expected a review");
    };
    review
}

fn error_kind(argv: &[&str]) -> ErrorKind {
    Ncli::try_parse_from(["ncli"].iter().chain(argv))
        .unwrap_err()
        .kind()
}

#[test]
fn command_definition_is_valid() {
    Ncli::command().debug_assert();
}

#[test]
fn review_takes_the_link_and_defaults_the_judge_to_claude() {
    let review = review(&[LINK]);
    assert_eq!(review.link.as_deref(), Some(LINK));
    assert_eq!(review.panel.judge, AgentKind::Claude);
}

#[test]
fn review_without_a_link_reviews_the_local_changes() {
    assert_eq!(review(&[]).link, None);
}

#[test]
fn judge_accepts_known_reviewers_only() {
    assert_eq!(
        review(&[LINK, "--judge", "codex"]).panel.judge,
        AgentKind::Codex
    );
    assert_eq!(
        error_kind(&["code", "review", LINK, "--judge", "gemini"]),
        ErrorKind::InvalidValue
    );
}

#[test]
fn repeated_flag_keeps_the_last_value() {
    let review = review(&[LINK, "--judge", "codex", "--judge", "claude"]);
    assert_eq!(review.panel.judge, AgentKind::Claude);
}

#[test]
fn unambiguous_flag_prefixes_are_accepted() {
    assert_eq!(
        review(&[LINK, "--jud", "codex"]).panel.judge,
        AgentKind::Codex
    );
}

#[test]
fn implement_takes_the_task_and_defaults_to_three_rounds_judged_by_claude() {
    let ncli = Ncli::try_parse_from(["ncli", "code", "implement", "add a flag"]).unwrap();
    let Command::Code(CodeCommand::Implement(implement)) = ncli.command else {
        panic!("expected an implement");
    };
    assert_eq!(implement.task, "add a flag");
    assert_eq!(implement.max_rounds, 5);
    assert_eq!(implement.panel.judge, AgentKind::Claude);
}

#[test]
fn implement_needs_a_task_and_at_least_one_round() {
    assert_eq!(
        error_kind(&["code", "implement"]),
        ErrorKind::MissingRequiredArgument
    );
    assert_eq!(
        error_kind(&["code", "implement", "add a flag", "--max-rounds", "0"]),
        ErrorKind::ValueValidation
    );
}
