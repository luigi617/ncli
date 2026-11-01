#![allow(dead_code)]

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{env, fs};

use ncli::process::{Call, Cli, Outcome, ProcessError, Tracer};
use tempfile::TempDir;

#[derive(Clone, Default)]
pub struct RecordingTracer {
    events: Arc<Mutex<Vec<(String, u32)>>>,
}

impl RecordingTracer {
    pub fn events(&self) -> Vec<(String, u32)> {
        self.events.lock().unwrap().clone()
    }

    pub fn count(&self, kind: &str) -> usize {
        self.events()
            .iter()
            .filter(|(event, _)| event == kind)
            .count()
    }

    fn record(&self, event: String, call: &Call) {
        self.events.lock().unwrap().push((event, call.attempt));
    }
}

impl Tracer for RecordingTracer {
    fn before_call(&self, call: &Call) {
        self.record("before".into(), call);
    }

    fn after_call(&self, call: &Call, outcome: &Outcome) {
        self.record(format!("after:{}", outcome.returncode()), call);
    }

    fn on_error(&self, call: &Call, _error: &ProcessError) {
        self.record("error".into(), call);
    }

    fn on_retry(&self, call: &Call, delay: Duration) {
        self.record(format!("retry:{}", delay.as_secs_f64()), call);
    }
}

pub fn events(expected: &[(&str, u32)]) -> Vec<(String, u32)> {
    expected
        .iter()
        .map(|(event, attempt)| (event.to_string(), *attempt))
        .collect()
}

pub fn ncli(state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ncli"));
    command.env(ncli::state::DIR_VAR, state);
    command
}

pub const CHILD: &str = env!("CARGO_BIN_EXE_ncli-test-child");

pub fn child_name() -> &'static str {
    Path::new(CHILD).file_name().unwrap().to_str().unwrap()
}

pub fn child(args: &[&str]) -> Cli {
    Cli::new([CHILD].iter().chain(args).copied())
}

pub fn flaky(dir: &Path, failures: u32) -> Cli {
    let counter = dir.join("count");
    child(&["flaky", counter.to_str().unwrap(), &failures.to_string()])
}

#[cfg(unix)]
pub fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

pub fn install(dir: &Path, names: &[&str]) {
    for name in names {
        #[cfg(windows)]
        fs::write(dir.join(format!("{name}.cmd")), "@echo off\n").unwrap();
        #[cfg(unix)]
        script(dir, name, "#!/bin/sh\n");
    }
}

static PATH_LOCK: Mutex<()> = Mutex::new(());

pub struct PathWith {
    pub dir: TempDir,
    _lock: MutexGuard<'static, ()>,
}

pub fn path_with(names: &[&str]) -> PathWith {
    let lock = PATH_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = TempDir::new().unwrap();
    install(dir.path(), names);
    unsafe { env::set_var("PATH", dir.path()) };
    PathWith { dir, _lock: lock }
}

pub struct Grandchild {
    dir: TempDir,
}

impl Grandchild {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
        }
    }

    pub fn dir(&self) -> &str {
        self.dir.path().to_str().unwrap()
    }

    pub fn started(&self) -> bool {
        self.dir.path().join("started").exists()
    }

    pub fn wait_until_started(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !self.started() {
            assert!(
                std::time::Instant::now() < deadline,
                "the grandchild never started"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn assert_survived(&self) {
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            self.dir.path().join("survived").exists(),
            "the grandchild was killed"
        );
    }

    pub fn assert_killed(&self) {
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            !self.dir.path().join("survived").exists(),
            "the grandchild outlived the call"
        );
    }
}
