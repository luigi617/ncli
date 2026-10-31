mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{RecordingTracer, child};
use ncli::process;

#[test]
fn cancel_kills_running_calls_without_retrying_and_later_spawns() {
    let tracer = RecordingTracer::default();
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(300));
        process::cancel();
    });
    let started = Instant::now();
    child(&["spawn-and-wait", "30"])
        .attempts(3)
        .tracer(tracer.clone())
        .run(None, None)
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(tracer.count("before"), 1);

    let started = Instant::now();
    child(&["sleep", "30"]).run(None, None).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
}
