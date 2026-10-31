mod common;

use std::fs;
use std::time::{Duration, Instant};

use common::{RecordingTracer, child, child_name, events, flaky};
use ncli::process::Cli;
use tempfile::TempDir;

#[test]
#[should_panic(expected = "attempts must be at least 1")]
fn attempts_must_be_positive() {
    let _ = Cli::new(["x"]).attempts(0);
}

#[test]
fn run_returns_stdout_and_passes_args_input_and_cwd() {
    let dir = TempDir::new().unwrap();
    let out = child(&["describe", "arg"])
        .run(Some("stdin"), Some(dir.path()))
        .unwrap();
    let [arg, input, cwd] = out.splitn(3, ' ').collect::<Vec<_>>()[..] else {
        panic!("unexpected output: {out}");
    };
    assert_eq!((arg, input), ("arg", "stdin"));
    assert_eq!(
        fs::canonicalize(cwd).unwrap(),
        fs::canonicalize(dir.path()).unwrap()
    );
}

#[test]
fn large_input_and_output_do_not_deadlock() {
    let input = "x".repeat(4 << 20);
    let out = child(&["cat"]).run(Some(&input), None).unwrap();
    assert_eq!(out, input);
}

#[test]
fn output_newlines_are_normalized() {
    let out = child(&["write", "a\r\nb\rc\n"]).run(None, None).unwrap();
    assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn nonzero_exit_raises_with_code_and_stderr() {
    let error = child(&["fail", "1", "boom"]).run(None, None).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{} exited with code 1: boom", child_name())
    );
}

#[test]
fn timeout_raises() {
    let error = child(&["sleep", "5"])
        .timeout(Duration::from_millis(200))
        .run(None, None)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{} timed out after 0.2s", child_name())
    );
}

#[test]
fn unstartable_executable_raises() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing");
    let error = Cli::new([missing.to_str().unwrap()])
        .run(None, None)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("missing could not be started:")
    );
}

#[test]
fn retries_until_success_and_traces_every_attempt() {
    let dir = TempDir::new().unwrap();
    let tracer = RecordingTracer::default();
    let cli = flaky(dir.path(), 2)
        .attempts(3)
        .retry_delay(Duration::from_millis(10))
        .tracer(tracer.clone());
    assert_eq!(cli.run(None, None).unwrap().trim(), "ok");
    assert_eq!(
        tracer.events(),
        events(&[
            ("before", 1),
            ("after:1", 1),
            ("error", 1),
            ("retry:0.01", 1),
            ("before", 2),
            ("after:1", 2),
            ("error", 2),
            ("retry:0.01", 2),
            ("before", 3),
            ("after:0", 3),
        ])
    );
}

#[test]
fn gives_up_after_the_last_attempt() {
    let dir = TempDir::new().unwrap();
    let tracer = RecordingTracer::default();
    let error = flaky(dir.path(), 5)
        .attempts(2)
        .tracer(tracer.clone())
        .run(None, None)
        .unwrap_err();
    assert!(error.to_string().contains("transient"));
    let errors: Vec<_> = tracer
        .events()
        .into_iter()
        .filter(|(event, _)| event == "error")
        .collect();
    assert_eq!(errors, events(&[("error", 1), ("error", 2)]));
}

#[test]
fn timeouts_are_not_retried() {
    let tracer = RecordingTracer::default();
    child(&["sleep", "5"])
        .timeout(Duration::from_millis(200))
        .attempts(3)
        .tracer(tracer.clone())
        .run(None, None)
        .unwrap_err();
    assert_eq!(tracer.events(), events(&[("before", 1), ("error", 1)]));
}

#[test]
fn unstartable_executable_is_not_retried() {
    let dir = TempDir::new().unwrap();
    let tracer = RecordingTracer::default();
    Cli::new([dir.path().join("missing").to_str().unwrap()])
        .attempts(3)
        .tracer(tracer.clone())
        .run(None, None)
        .unwrap_err();
    assert_eq!(tracer.events(), events(&[("before", 1), ("error", 1)]));
}

#[cfg(unix)]
#[test]
fn killed_by_signal_is_reported_and_not_retried() {
    let tracer = RecordingTracer::default();
    let error = Cli::new(["sh", "-c", "kill -TERM $$"])
        .attempts(3)
        .tracer(tracer.clone())
        .run(None, None)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("sh was killed by signal {}", libc::SIGTERM)
    );
    assert_eq!(tracer.count("before"), 1);
}

#[test]
fn timeout_kills_children_holding_the_output_pipe() {
    let started = Instant::now();
    let error = child(&["spawn-and-wait", "30"])
        .timeout(Duration::from_millis(300))
        .run(None, None)
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn error_message_keeps_only_the_end_of_stderr() {
    let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    let args: Vec<&str> = ["fail", "1"]
        .into_iter()
        .chain(lines.iter().map(String::as_str))
        .collect();
    let message = child(&args).run(None, None).unwrap_err().to_string();
    assert!(message.contains("line 99") && message.contains("line 80"));
    assert!(!message.contains("line 79"));
}

#[test]
fn carriage_returns_split_the_stderr_tail() {
    let progress: Vec<String> = (0..100).map(|i| format!("step {i}")).collect();
    let message = child(&["fail", "1", &progress.join("\r")])
        .run(None, None)
        .unwrap_err()
        .to_string();
    assert!(message.contains("step 99") && message.contains("step 80"));
    assert!(!message.contains("step 79") && !message.contains('\r'));
}
