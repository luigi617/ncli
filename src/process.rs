use std::borrow::Cow;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::Error;

const STDERR_TAIL_LINES: usize = 20;
const LINE_SEPARATOR: &str = if cfg!(windows) { "\r\n" } else { "\n" };

struct State {
    cancelled: bool,
    running: BTreeSet<u32>,
}

static STATE: Mutex<State> = Mutex::new(State {
    cancelled: false,
    running: BTreeSet::new(),
});
static CANCELLED: Condvar = Condvar::new();

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{name} could not be started: {source}")]
    Spawn { name: String, source: io::Error },
    #[error("{name} failed: {source}")]
    Io { name: String, source: io::Error },
    #[error("{name} timed out after {}s", .timeout.as_secs_f64())]
    Timeout { name: String, timeout: Duration },
    #[error("{name} was killed by signal {signal}")]
    Signal { name: String, signal: i32 },
    #[error("{name} exited with code {code}{}", detail(.stderr_tail))]
    Exit {
        name: String,
        code: i32,
        stderr_tail: String,
    },
}

impl ProcessError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Exit { .. })
    }
}

fn detail(stderr_tail: &str) -> String {
    if stderr_tail.is_empty() {
        String::new()
    } else {
        format!(": {stderr_tail}")
    }
}

pub fn require_executables<'a>(names: impl IntoIterator<Item = &'a OsStr>) -> Result<(), Error> {
    let mut missing: Vec<OsString> = Vec::new();
    for name in names {
        if !missing.iter().any(|seen| seen == name) && which::which(name).is_err() {
            missing.push(name.to_owned());
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::MissingExecutables(missing))
    }
}

pub fn cancel() {
    let mut state = state();
    state.cancelled = true;
    for &pid in &state.running {
        kill(pid);
    }
    CANCELLED.notify_all();
}

pub fn is_cancelled() -> bool {
    state().cancelled
}

fn sleep_unless_cancelled(delay: Duration) -> bool {
    let (state, _) = CANCELLED
        .wait_timeout_while(state(), delay, |state| !state.cancelled)
        .unwrap_or_else(PoisonError::into_inner);
    state.cancelled
}

fn track(pid: u32) {
    let mut state = state();
    state.running.insert(pid);
    if state.cancelled {
        kill(pid);
    }
}

fn untrack(pid: u32) {
    state().running.remove(&pid);
}

#[cfg(unix)]
fn new_group(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn new_group(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn kill(pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .output();
}

#[cfg(unix)]
fn signal(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(windows)]
fn signal(_: ExitStatus) -> Option<i32> {
    None
}

pub struct Call<'a> {
    pub argv: &'a [OsString],
    pub attempt: u32,
    pub attempts: u32,
}

impl Call<'_> {
    pub fn name(&self) -> Cow<'_, str> {
        let program = &self.argv[0];
        Path::new(program)
            .file_name()
            .unwrap_or(program)
            .to_string_lossy()
    }
}

pub struct Outcome {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

impl Outcome {
    pub fn returncode(&self) -> i32 {
        match signal(self.status) {
            Some(signal) => -signal,
            None => self
                .status
                .code()
                .expect("a process not killed by a signal has an exit code"),
        }
    }
}

pub trait Tracer: Send + Sync {
    fn before_call(&self, _call: &Call) {}
    fn after_call(&self, _call: &Call, _outcome: &Outcome) {}
    fn on_error(&self, _call: &Call, _error: &ProcessError) {}
    fn on_retry(&self, _call: &Call, _error: &ProcessError, _delay: Duration) {}
}

struct SilentTracer;

impl Tracer for SilentTracer {}

pub struct StderrTracer;

impl StderrTracer {
    fn log(call: &Call, message: &str) {
        eprintln!(
            "ncli: {} (attempt {}/{}) {message}",
            call.name(),
            call.attempt,
            call.attempts
        );
    }
}

impl Tracer for StderrTracer {
    fn before_call(&self, call: &Call) {
        Self::log(call, "started");
    }

    fn after_call(&self, call: &Call, outcome: &Outcome) {
        let seconds = outcome.duration.as_secs_f64();
        let code = outcome.returncode();
        Self::log(
            call,
            &format!("exited with code {code} after {seconds:.1}s"),
        );
    }

    fn on_error(&self, call: &Call, error: &ProcessError) {
        Self::log(call, &format!("failed: {error}"));
    }

    fn on_retry(&self, call: &Call, _error: &ProcessError, delay: Duration) {
        Self::log(call, &format!("retrying in {}s", delay.as_secs_f64()));
    }
}

fn failure(call: &Call, outcome: &Outcome) -> ProcessError {
    let name = call.name().into_owned();
    if let Some(signal) = signal(outcome.status) {
        return ProcessError::Signal { name, signal };
    }
    let lines: Vec<&str> = outcome.stderr.trim().lines().collect();
    ProcessError::Exit {
        name,
        code: outcome.returncode(),
        stderr_tail: lines[lines.len().saturating_sub(STDERR_TAIL_LINES)..].join("\n"),
    }
}

fn universal_newlines(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn feed(mut stdin: ChildStdin, input: &str) -> io::Result<()> {
    let input = input.replace('\n', LINE_SEPARATOR);
    match stdin.write_all(input.as_bytes()) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

fn communicate(
    mut child: Child,
    input: Option<&str>,
    timeout: Option<Duration>,
) -> io::Result<Option<Output>> {
    let pid = child.id();
    thread::scope(|scope| {
        let writer = child
            .stdin
            .take()
            .zip(input)
            .map(|(stdin, input)| scope.spawn(move || feed(stdin, input)));
        let (sender, receiver) = mpsc::channel();
        let waiter = scope.spawn(move || sender.send(child.wait_with_output()));
        let received = match timeout {
            Some(timeout) => receiver.recv_timeout(timeout),
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let output = match received {
            Ok(output) => Some(output?),
            Err(RecvTimeoutError::Timeout) => {
                kill(pid);
                None
            }
            Err(RecvTimeoutError::Disconnected) => std::panic::resume_unwind(
                waiter
                    .join()
                    .expect_err("the waiter only disconnects by panicking"),
            ),
        };
        if let Some(writer) = writer {
            writer.join().expect("stdin writer panicked")?;
        }
        Ok(output)
    })
}

pub struct Cli {
    argv: Vec<OsString>,
    timeout: Option<Duration>,
    attempts: u32,
    retry_delay: Duration,
    tracer: Box<dyn Tracer>,
}

impl Cli {
    pub fn new(argv: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            timeout: None,
            attempts: 1,
            retry_delay: Duration::ZERO,
            tracer: Box::new(SilentTracer),
        }
    }

    pub fn timeout(self, timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            ..self
        }
    }

    pub fn attempts(self, attempts: u32) -> Self {
        assert!(attempts >= 1, "attempts must be at least 1, got {attempts}");
        Self { attempts, ..self }
    }

    pub fn retry_delay(self, retry_delay: Duration) -> Self {
        Self {
            retry_delay,
            ..self
        }
    }

    pub fn tracer(self, tracer: impl Tracer + 'static) -> Self {
        Self {
            tracer: Box::new(tracer),
            ..self
        }
    }

    pub fn executable(&self) -> &OsStr {
        &self.argv[0]
    }

    pub fn run(&self, input: Option<&str>, cwd: Option<&Path>) -> Result<String, ProcessError> {
        let call = |attempt| Call {
            argv: &self.argv,
            attempt,
            attempts: self.attempts,
        };
        for attempt in 1..self.attempts {
            let call = call(attempt);
            match self.attempt(&call, input, cwd) {
                Err(error) if error.is_retryable() && !is_cancelled() => {
                    self.tracer.on_retry(&call, &error, self.retry_delay);
                    if sleep_unless_cancelled(self.retry_delay) {
                        return Err(error);
                    }
                }
                result => return result,
            }
        }
        self.attempt(&call(self.attempts), input, cwd)
    }

    fn attempt(
        &self,
        call: &Call,
        input: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<String, ProcessError> {
        self.tracer.before_call(call);
        let result = self.execute(call, input, cwd).and_then(|outcome| {
            self.tracer.after_call(call, &outcome);
            if outcome.status.success() {
                Ok(outcome.stdout)
            } else {
                Err(failure(call, &outcome))
            }
        });
        if let Err(error) = &result {
            self.tracer.on_error(call, error);
        }
        result
    }

    fn execute(
        &self,
        call: &Call,
        input: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<Outcome, ProcessError> {
        let start = Instant::now();
        let program = which::which(&call.argv[0]).unwrap_or_else(|_| PathBuf::from(&call.argv[0]));
        let mut command = std::process::Command::new(program);
        command
            .args(&call.argv[1..])
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        new_group(&mut command);
        let name = || call.name().into_owned();
        let child = command.spawn().map_err(|source| ProcessError::Spawn {
            name: name(),
            source,
        })?;
        let pid = child.id();
        track(pid);
        let communicated = communicate(child, input, self.timeout);
        untrack(pid);
        match communicated {
            Ok(Some(output)) => Ok(Outcome {
                status: output.status,
                stdout: universal_newlines(&output.stdout),
                stderr: universal_newlines(&output.stderr),
                duration: start.elapsed(),
            }),
            Ok(None) => Err(ProcessError::Timeout {
                name: name(),
                timeout: self.timeout.expect("only a timeout stops the wait early"),
            }),
            Err(source) => Err(ProcessError::Io {
                name: name(),
                source,
            }),
        }
    }
}
