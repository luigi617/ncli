mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{RecordingTracer, flaky};
use ncli::process;
use tempfile::TempDir;

#[test]
fn cancel_stops_pending_retries() {
    let dir = TempDir::new().unwrap();
    let tracer = RecordingTracer::default();
    let cli = flaky(dir.path(), 1)
        .attempts(3)
        .retry_delay(Duration::from_secs(30))
        .tracer(tracer.clone());
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(500));
        process::cancel();
    });
    let started = Instant::now();
    let error = cli.run(None, None).unwrap_err();
    assert!(error.to_string().contains("transient"));
    assert!(started.elapsed() < Duration::from_secs(5));
    let kinds: Vec<_> = tracer
        .events()
        .into_iter()
        .map(|(event, _)| event)
        .collect();
    assert_eq!(kinds, ["before", "after:1", "error", "retry:30"]);
}
