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
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{Duration, Instant};

pub use cancel::Cancellation;

use crate::stderr;

const TAIL_LINES: usize = 20;
// Like Go's exec.Cmd.WaitDelay: once the child exits, a process that escaped it may still hold the
// pipes open, so they are closed after this long and the output is reported as incomplete.
const WAIT_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{name} could not be started: {source}")]
    Spawn { name: String, source: io::Error },
    #[error("{name} failed: {source}")]
    Io { name: String, source: io::Error },
    #[error("{name} timed out after {}s", .timeout.as_secs_f64())]
    Timeout {
        name: String,
        timeout: Duration,
        stdout: String,
    },
    #[error("{name} was cancelled")]
    Cancelled { name: String, stdout: String },
    #[error("{name} exited, but a process it started kept its output open")]
    OutputHeldOpen { name: String, stdout: String },
    #[error("{name} was killed by signal {signal}{}", detail(.stderr_tail))]
    Signal {
        name: String,
        signal: i32,
        stderr_tail: String,
        stdout: String,
    },
    #[error("{name} exited with code {code}{}", detail(.stderr_tail))]
    Exit {
        name: String,
        code: i32,
        stderr_tail: String,
        stdout: String,
    },
}

impl ProcessError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Exit { .. })
    }

    pub fn stdout(&self) -> &str {
        match self {
            Self::Spawn { .. } | Self::Io { .. } => "",
            Self::Timeout { stdout, .. }
            | Self::Cancelled { stdout, .. }
            | Self::OutputHeldOpen { stdout, .. }
            | Self::Signal { stdout, .. }
            | Self::Exit { stdout, .. } => stdout,
        }
    }
}

fn detail(stderr_tail: &str) -> String {
    if stderr_tail.is_empty() {
        String::new()
    } else {
        format!(": {stderr_tail}")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("required executables not found on PATH: {}", join_lossy(.0))]
pub struct MissingExecutables(pub Vec<OsString>);

#[derive(Debug, thiserror::Error)]
#[error("none of these executables were found on PATH: {}", join_lossy(.0))]
pub struct NoExecutable(pub Vec<OsString>);

fn join_lossy(names: &[OsString]) -> String {
    names
        .iter()
        .map(|name| name.to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn last_lines(text: &str) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

pub fn require_executables<'a>(
    names: impl IntoIterator<Item = &'a OsStr>,
) -> Result<(), MissingExecutables> {
    let mut missing: Vec<OsString> = Vec::new();
    for name in names {
        if !missing.iter().any(|seen| seen == name) && which::which(name).is_err() {
            missing.push(name.to_owned());
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MissingExecutables(missing))
    }
}

pub fn first_available<T>(
    candidates: impl IntoIterator<Item = T>,
    executable: impl Fn(&T) -> &OsStr,
) -> Result<T, NoExecutable> {
    let mut tried = Vec::new();
    for candidate in candidates {
        if which::which(executable(&candidate)).is_ok() {
            return Ok(candidate);
        }
        tried.push(executable(&candidate).to_owned());
    }
    Err(NoExecutable(tried))
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
    fn on_retry(&self, _call: &Call, _delay: Duration) {}
}

struct SilentTracer;

impl Tracer for SilentTracer {}

pub struct StderrTracer;

impl StderrTracer {
    fn log(call: &Call, message: &str) {
        stderr::line(format_args!(
            "ncli: {} (attempt {}/{}) {message}",
            call.name(),
            call.attempt,
            call.attempts
        ));
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

    fn on_retry(&self, call: &Call, delay: Duration) {
        Self::log(call, &format!("retrying in {}s", delay.as_secs_f64()));
    }
}

fn failure(call: &Call, outcome: Outcome) -> ProcessError {
    let name = call.name().into_owned();
    if let Some(signal) = sys::signal(outcome.status) {
        return ProcessError::Signal {
            name,
            signal,
            stderr_tail: last_lines(&outcome.stderr),
            stdout: outcome.stdout,
        };
    }
    ProcessError::Exit {
        name,
        code: outcome.returncode(),
        stderr_tail: last_lines(&outcome.stderr),
        stdout: outcome.stdout,
    }
}

// Best effort, like ncli's own diagnostics: a closed stderr must not fail the command.
fn echo(output: &[u8]) {
    let _ = io::Write::write_all(&mut io::stderr().lock(), output);
}

fn universal_newlines(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

#[derive(Clone, Copy)]
enum Stdio {
    Piped {
        piped_stdin: bool,
        echo: bool,
    },
    #[cfg(unix)]
    Detached {
        piped_stdin: bool,
    },
    Terminal,
}

struct Communicated {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
    output_closed: bool,
}

pub struct Cli {
    argv: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    timeout: Option<Duration>,
    attempts: u32,
    retry_delay: Duration,
    tracer: Box<dyn Tracer>,
    terminal: bool,
    echo: bool,
    #[cfg(unix)]
    leave_running: bool,
}

impl Cli {
    pub fn new(argv: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        let argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
        assert!(!argv.is_empty(), "argv must name an executable");
        Self {
            argv,
            envs: Vec::new(),
            timeout: None,
            attempts: 1,
            retry_delay: Duration::ZERO,
            tracer: Box::new(SilentTracer),
            terminal: false,
            echo: false,
            #[cfg(unix)]
            leave_running: false,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.argv.push(arg.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
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

    // For an interactive command, such as one that attaches to a terminal multiplexer. It shares
    // ncli's terminal, so its output is not captured and it gets no input from `run`.
    pub fn terminal(self) -> Self {
        Self {
            terminal: true,
            ..self
        }
    }

    // For a command whose output the user follows as it runs, such as a test suite. Its output is
    // still captured, and is also copied to ncli's stderr, which keeps ncli's stdout for its result.
    pub fn echo(self) -> Self {
        Self { echo: true, ..self }
    }

    // For a command that must leave a process running, such as a clipboard owner. That process would
    // hold the output pipes open, so the output is discarded.
    #[cfg(unix)]
    pub fn leave_running(self) -> Self {
        Self {
            leave_running: true,
            ..self
        }
    }

    fn stdio(&self, piped_stdin: bool) -> Stdio {
        if self.terminal {
            return Stdio::Terminal;
        }
        #[cfg(unix)]
        if self.leave_running {
            return Stdio::Detached { piped_stdin };
        }
        Stdio::Piped {
            piped_stdin,
            echo: self.echo,
        }
    }

    pub fn executable(&self) -> &OsStr {
        &self.argv[0]
    }

    pub fn run(
        &self,
        args: &[OsString],
        input: Option<&[u8]>,
        cwd: Option<&Path>,
        cancellation: &Cancellation,
    ) -> Result<String, ProcessError> {
        assert!(
            !(self.terminal && input.is_some()),
            "a command run in the terminal reads its input from the terminal"
        );
        let argv: Vec<OsString> = self.argv.iter().chain(args).cloned().collect();
        let call = |attempt| Call {
            argv: &argv,
            attempt,
            attempts: self.attempts,
        };
        for attempt in 1..self.attempts {
            let call = call(attempt);
            match self.attempt(&call, input, cwd, cancellation) {
                Err(error) if error.is_retryable() && !cancellation.is_cancelled() => {
                    self.tracer.on_retry(&call, self.retry_delay);
                    if cancellation.sleep(self.retry_delay) {
                        return Err(ProcessError::Cancelled {
                            name: call.name().into_owned(),
                            stdout: error.stdout().to_owned(),
                        });
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
        input: Option<&[u8]>,
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
                    Err(failure(call, outcome))
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
        input: Option<&[u8]>,
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
            .envs(self.envs.iter().map(|(k, v)| (k, v)));
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut process =
            sys::Process::spawn(command, self.stdio(input.is_some())).map_err(|source| {
                ProcessError::Spawn {
                    name: name(),
                    source,
                }
            })?;
        let registration = cancellation.register(process.group());
        let communicated = process.communicate(input, self.timeout);
        drop(registration);
        let status = process.wait().map_err(io_error)?;
        let communicated = communicated.map_err(io_error)?;
        let stdout = universal_newlines(&communicated.stdout);
        if communicated.timed_out {
            return Err(ProcessError::Timeout {
                name: name(),
                timeout: self.timeout.expect("only a timeout stops the wait early"),
                stdout,
            });
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::Cancelled {
                name: name(),
                stdout,
            });
        }
        if status.success() && !communicated.output_closed {
            return Err(ProcessError::OutputHeldOpen {
                name: name(),
                stdout,
            });
        }
        Ok(Outcome {
            status,
            stdout,
            stderr: universal_newlines(&communicated.stderr),
            duration: start.elapsed(),
        })
    }
}
