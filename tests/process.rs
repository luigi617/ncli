mod common;

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::{Grandchild, RecordingTracer, child, child_name, events, flaky};
use ncli::process::{Cancellation, Cli, ProcessError};
use tempfile::TempDir;

#[test]
#[should_panic(expected = "attempts must be at least 1")]
fn attempts_must_be_positive() {
    let _ = Cli::new(["x"]).attempts(0);
}

#[test]
fn run_returns_stdout_and_passes_argv_input_and_cwd() {
    let dir = TempDir::new().unwrap();
    let out = child(&["describe", "arg"])
        .run(Some("stdin"), Some(dir.path()), &Cancellation::default())
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
    let out = child(&["cat"])
        .run(Some(&input), None, &Cancellation::default())
        .unwrap();
    assert_eq!(out, input);
}

#[test]
fn output_newlines_are_normalized() {
    let out = child(&["write", "a\r\nb\rc\n"])
        .run(None, None, &Cancellation::default())
        .unwrap();
    assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn nonzero_exit_raises_with_code_and_stderr() {
    let error = child(&["fail", "1", "boom"])
        .run(None, None, &Cancellation::default())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{} exited with code 1: boom", child_name())
    );
}

#[test]
fn timeout_raises() {
    let error = child(&["sleep", "5"])
        .timeout(Duration::from_millis(200))
        .run(None, None, &Cancellation::default())
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
        .run(None, None, &Cancellation::default())
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
    assert_eq!(
        cli.run(None, None, &Cancellation::default())
            .unwrap()
            .trim(),
        "ok"
    );
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
        .run(None, None, &Cancellation::default())
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
        .run(None, None, &Cancellation::default())
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
        .run(None, None, &Cancellation::default())
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
        .run(None, None, &Cancellation::default())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("sh was killed by signal {}", libc::SIGTERM)
    );
    assert_eq!(tracer.count("before"), 1);
}

#[test]
fn timeout_kills_the_whole_process_group() {
    let grandchild = Grandchild::new();
    let error = child(&["spawn-and-wait", grandchild.dir()])
        .timeout(Duration::from_millis(1500))
        .run(None, None, &Cancellation::default())
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(
        grandchild.started(),
        "the timeout fired before the grandchild started"
    );
    grandchild.assert_killed();
}

#[test]
fn error_message_keeps_only_the_end_of_stderr() {
    let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    let args: Vec<&str> = ["fail", "1"]
        .into_iter()
        .chain(lines.iter().map(String::as_str))
        .collect();
    let message = child(&args)
        .run(None, None, &Cancellation::default())
        .unwrap_err()
        .to_string();
    assert!(message.contains("line 99") && message.contains("line 80"));
    assert!(!message.contains("line 79"));
}

#[test]
fn carriage_returns_split_the_stderr_tail() {
    let progress: Vec<String> = (0..100).map(|i| format!("step {i}")).collect();
    let message = child(&["fail", "1", &progress.join("\r")])
        .run(None, None, &Cancellation::default())
        .unwrap_err()
        .to_string();
    assert!(message.contains("step 99") && message.contains("step 80"));
    assert!(!message.contains("step 79") && !message.contains('\r'));
}

#[test]
#[should_panic(expected = "argv must name an executable")]
fn argv_must_not_be_empty() {
    let _ = Cli::new(Vec::<String>::new());
}

#[test]
fn input_is_passed_unchanged() {
    let out = child(&["bytes"])
        .run(Some("a\nb\r\n"), None, &Cancellation::default())
        .unwrap();
    assert_eq!(out, format!("{:?}", b"a\nb\r\n"));
}

#[test]
fn processes_left_behind_by_the_child_are_killed() {
    let grandchild = Grandchild::new();
    child(&["spawn-and-exit", grandchild.dir()])
        .run(None, None, &Cancellation::default())
        .unwrap();
    grandchild.assert_killed();
}

#[cfg(unix)]
#[test]
fn output_held_open_by_an_escaped_process_is_an_error_and_is_closed() {
    let dir = TempDir::new().unwrap();
    let started = Instant::now();
    let error = child(&["escape", dir.path().to_str().unwrap()])
        .run(None, None, &Cancellation::default())
        .unwrap_err();
    assert!(
        matches!(error, ProcessError::OutputHeldOpen { .. }),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    thread::sleep(Duration::from_millis(500));
    assert!(
        dir.path().join("closed").exists(),
        "the escaped process can still write to the output pipe"
    );
}

#[cfg(windows)]
#[test]
fn nothing_escapes_the_job() {
    let dir = TempDir::new().unwrap();
    let out = child(&["escape", dir.path().to_str().unwrap()])
        .run(None, None, &Cancellation::default())
        .unwrap();
    assert!(out.contains("done"), "{out}");
}

fn cancel_after(cancellation: &Cancellation, delay: Duration) {
    let cancellation = cancellation.clone();
    thread::spawn(move || {
        thread::sleep(delay);
        cancellation.cancel();
    });
}

#[test]
fn cancel_kills_running_calls_without_retrying() {
    let cancellation = Cancellation::default();
    let tracer = RecordingTracer::default();
    let grandchild = Arc::new(Grandchild::new());
    let started = Arc::clone(&grandchild);
    let canceller = cancellation.clone();
    thread::spawn(move || {
        started.wait_until_started();
        canceller.cancel();
    });
    let error = child(&["spawn-and-wait", grandchild.dir()])
        .attempts(3)
        .tracer(tracer.clone())
        .run(None, None, &cancellation)
        .unwrap_err();
    assert!(matches!(error, ProcessError::Cancelled { .. }), "{error}");
    assert_eq!(error.to_string(), format!("{} was cancelled", child_name()));
    assert_eq!(tracer.count("before"), 1);
    grandchild.assert_killed();
}

#[test]
fn calls_started_after_cancel_are_killed_immediately() {
    let cancellation = Cancellation::default();
    cancellation.cancel();
    let started = Instant::now();
    let error = child(&["sleep", "30"])
        .run(None, None, &cancellation)
        .unwrap_err();
    assert!(matches!(error, ProcessError::Cancelled { .. }), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn cancel_stops_pending_retries() {
    let dir = TempDir::new().unwrap();
    let cancellation = Cancellation::default();
    let tracer = RecordingTracer::default();
    let cli = flaky(dir.path(), 1)
        .attempts(3)
        .retry_delay(Duration::from_secs(30))
        .tracer(tracer.clone());
    cancel_after(&cancellation, Duration::from_millis(500));
    let started = Instant::now();
    let error = cli.run(None, None, &cancellation).unwrap_err();
    assert!(error.to_string().contains("transient"));
    assert!(started.elapsed() < Duration::from_secs(5));
    let kinds: Vec<_> = tracer
        .events()
        .into_iter()
        .map(|(event, _)| event)
        .collect();
    assert_eq!(kinds, ["before", "after:1", "error", "retry:30"]);
}

#[test]
fn cancellation_propagates_from_parent_to_children_only() {
    let parent = Cancellation::default();
    let (first, second) = (parent.child(), parent.child());
    let unrelated = Cancellation::default();
    first.cancel();
    assert!(!parent.is_cancelled() && !second.is_cancelled());
    parent.cancel();
    assert!(second.is_cancelled() && parent.child().is_cancelled());
    assert!(!unrelated.is_cancelled());
}
