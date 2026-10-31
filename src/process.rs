mod cancel;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
use windows as sys;

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

pub use cancel::Cancellation;

use crate::error::Error;

const STDERR_TAIL_LINES: usize = 20;
// Like Go's exec.Cmd.WaitDelay: after the child exits, a process that escaped its group may still
// hold the output pipes open, so stop waiting for end-of-file after this long.
const WAIT_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{name} could not be started: {source}")]
    Spawn { name: String, source: io::Error },
    #[error("{name} failed: {source}")]
    Io { name: String, source: io::Error },
    #[error("{name} timed out after {}s", .timeout.as_secs_f64())]
    Timeout { name: String, timeout: Duration },
    #[error("{name} was cancelled")]
    Cancelled { name: String },
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
        match sys::signal(self.status) {
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
    if let Some(signal) = sys::signal(outcome.status) {
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

struct Capture {
    output: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<io::Result<()>>,
}

impl Capture {
    fn start(mut source: impl Read + Send + 'static) -> Self {
        let output = Arc::new(Mutex::new(Vec::new()));
        let (sender, done) = mpsc::channel();
        let sink = Arc::clone(&output);
        thread::spawn(move || {
            let mut chunk = [0; 8192];
            let result = loop {
                match source.read(&mut chunk) {
                    Ok(0) => break Ok(()),
                    Ok(read) => lock(&sink).extend_from_slice(&chunk[..read]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => break Err(error),
                }
            };
            let _ = sender.send(result);
        });
        Self { output, done }
    }

    fn finish(self, deadline: Instant) -> io::Result<String> {
        settle(&self.done, deadline)?;
        Ok(universal_newlines(&lock(&self.output)))
    }
}

fn feed(mut stdin: ChildStdin, input: String) -> mpsc::Receiver<io::Result<()>> {
    let (sender, done) = mpsc::channel();
    thread::spawn(move || {
        let result = match stdin.write_all(input.as_bytes()) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            result => result,
        };
        let _ = sender.send(result);
    });
    done
}

fn settle(done: &mpsc::Receiver<io::Result<()>>, deadline: Instant) -> io::Result<()> {
    match done.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Ok(()),
        Err(RecvTimeoutError::Disconnected) => panic!("a pipe thread panicked"),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn wait_for_exit(child: &Child, group: &sys::Group, timeout: Option<Duration>) -> io::Result<bool> {
    thread::scope(|scope| {
        let (sender, exited) = mpsc::channel();
        let waiter = scope.spawn(move || {
            let result = sys::wait_for_exit(child);
            let _ = sender.send(());
            result
        });
        let timed_out = timeout
            .is_some_and(|timeout| exited.recv_timeout(timeout) == Err(RecvTimeoutError::Timeout));
        if timed_out {
            group.kill();
        }
        let result = waiter
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        result.map(|()| timed_out)
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
        let argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
        assert!(!argv.is_empty(), "argv must name an executable");
        Self {
            argv,
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

    pub fn run(
        &self,
        input: Option<&str>,
        cwd: Option<&Path>,
        cancellation: &Cancellation,
    ) -> Result<String, ProcessError> {
        let call = |attempt| Call {
            argv: &self.argv,
            attempt,
            attempts: self.attempts,
        };
        for attempt in 1..self.attempts {
            let call = call(attempt);
            match self.attempt(&call, input, cwd, cancellation) {
                Err(error) if error.is_retryable() && !cancellation.is_cancelled() => {
                    self.tracer.on_retry(&call, &error, self.retry_delay);
                    if cancellation.sleep(self.retry_delay) {
                        return Err(error);
                    }
                }
                result => return result,
            }
        }
        self.attempt(&call(self.attempts), input, cwd, cancellation)
    }

    fn attempt(
        &self,
        call: &Call,
        input: Option<&str>,
        cwd: Option<&Path>,
        cancellation: &Cancellation,
    ) -> Result<String, ProcessError> {
        self.tracer.before_call(call);
        let result = self
            .execute(call, input, cwd, cancellation)
            .and_then(|outcome| {
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
        cancellation: &Cancellation,
    ) -> Result<Outcome, ProcessError> {
        let start = Instant::now();
        let name = || call.name().into_owned();
        let io_error = |source| ProcessError::Io {
            name: name(),
            source,
        };
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
        sys::configure(&mut command);
        let spawn_error = |source| ProcessError::Spawn {
            name: name(),
            source,
        };
        let mut child = command.spawn().map_err(spawn_error)?;
        let group = Arc::new(sys::Group::new(&mut child).map_err(spawn_error)?);
        let stdout = Capture::start(child.stdout.take().expect("stdout is piped"));
        let stderr = Capture::start(child.stderr.take().expect("stderr is piped"));
        let fed = child
            .stdin
            .take()
            .zip(input)
            .map(|(stdin, input)| feed(stdin, input.to_owned()));
        let registration = cancellation.register(Arc::clone(&group));
        let exited = wait_for_exit(&child, &group, self.timeout);
        // Nothing the child started may outlive it; the group must be killed before the child is reaped.
        group.kill();
        drop(registration);
        let status = child.wait().map_err(io_error)?;
        let timed_out = exited.map_err(io_error)?;
        let deadline = Instant::now() + WAIT_DELAY;
        let stdout = stdout.finish(deadline).map_err(io_error)?;
        let stderr = stderr.finish(deadline).map_err(io_error)?;
        if let Some(fed) = fed {
            settle(&fed, deadline).map_err(io_error)?;
        }
        if timed_out {
            return Err(ProcessError::Timeout {
                name: name(),
                timeout: self.timeout.expect("only a timeout stops the wait early"),
            });
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::Cancelled { name: name() });
        }
        Ok(Outcome {
            status,
            stdout,
            stderr,
            duration: start.elapsed(),
        })
    }
}
