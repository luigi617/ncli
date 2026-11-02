use std::io::{self, PipeReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{
    Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio as StdStdio,
};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{Communicated, Stdio, WAIT_DELAY};

pub struct Process {
    child: Child,
    group: Arc<Group>,
    stdio: Stdio,
}

impl Process {
    pub fn spawn(mut command: Command, stdio: Stdio) -> io::Result<Self> {
        type Make = fn() -> StdStdio;
        let input = |piped: bool| -> Make {
            if piped {
                StdStdio::piped
            } else {
                StdStdio::null
            }
        };
        let (stdin, output): (Make, Make) = match stdio {
            Stdio::Piped { piped_stdin } => (input(piped_stdin), StdStdio::piped),
            Stdio::Detached { piped_stdin } => (input(piped_stdin), StdStdio::null),
            Stdio::Terminal => (StdStdio::inherit, StdStdio::inherit),
        };
        command.stdin(stdin()).stdout(output()).stderr(output());
        let own_session = !matches!(stdio, Stdio::Terminal);
        // A new session detaches the child from ncli's terminal, so a terminal prompt fails
        // instead of stopping the child until the timeout. A command run in the terminal stays in
        // ncli's foreground process group, which is what lets it read from the terminal.
        if own_session {
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = command.spawn()?;
        let group = Arc::new(Group::new(&child, own_session)?);
        Ok(Self {
            child,
            group,
            stdio,
        })
    }

    pub fn group(&self) -> Arc<Group> {
        Arc::clone(&self.group)
    }

    pub fn communicate(
        &mut self,
        input: Option<&[u8]>,
        timeout: Option<Duration>,
    ) -> io::Result<Communicated> {
        let group = &*self.group;
        let mut pipes =
            Pipes::new(&mut self.child, input.unwrap_or_default()).inspect_err(|_| group.kill())?;
        let (exit_reader, exit_writer) = io::pipe().inspect_err(|_| group.kill())?;
        let (child, kill_on_exit) = (&self.child, matches!(self.stdio, Stdio::Piped { .. }));
        thread::scope(|scope| {
            let waiter = scope.spawn(move || {
                let result = wait_for_exit(child);
                drop(exit_writer);
                result
            });
            let pumped = pipes.pump(&exit_reader, group, timeout, kill_on_exit);
            if pumped.is_err() {
                group.kill();
            }
            let waited = waiter
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            let (timed_out, output_closed) = pumped?;
            waited?;
            Ok(Communicated {
                stdout: pipes.stdout.output,
                stderr: pipes.stderr.output,
                timed_out,
                output_closed,
            })
        })
    }

    pub fn wait(mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }
}

pub fn signal(status: ExitStatus) -> Option<i32> {
    status.signal()
}

pub struct Group {
    id: libc::pid_t,
    whole: bool,
}

impl Group {
    fn new(child: &Child, whole: bool) -> io::Result<Self> {
        let id = libc::pid_t::try_from(child.id()).map_err(io::Error::other)?;
        Ok(Self { id, whole })
    }

    // Only valid while the leader is unreaped: until then the kernel cannot reuse its id. A child
    // that shares ncli's process group is killed alone, as killing the group would kill ncli.
    pub fn kill(&self) {
        unsafe {
            if self.whole {
                libc::killpg(self.id, libc::SIGKILL);
            } else {
                libc::kill(self.id, libc::SIGKILL);
            }
        }
    }
}

fn wait_for_exit(child: &Child) -> io::Result<()> {
    let id = libc::id_t::try_from(child.id()).map_err(io::Error::other)?;
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result =
            unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

struct Output<P> {
    pipe: Option<P>,
    output: Vec<u8>,
}

impl<P: Read + AsRawFd> Output<P> {
    fn new(pipe: Option<P>) -> io::Result<Self> {
        if let Some(pipe) = &pipe {
            set_nonblocking(pipe)?;
        }
        Ok(Self {
            pipe,
            output: Vec::new(),
        })
    }

    // One read per poll keeps a child that writes faster than it is read from starving the deadline.
    fn read(&mut self) -> io::Result<()> {
        let Some(pipe) = &mut self.pipe else {
            return Ok(());
        };
        let mut chunk = [0; 1 << 16];
        match pipe.read(&mut chunk) {
            Ok(0) => self.pipe = None,
            Ok(read) => self.output.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }
}

struct Pipes<'a> {
    stdin: Option<ChildStdin>,
    input: &'a [u8],
    stdout: Output<ChildStdout>,
    stderr: Output<ChildStderr>,
}

impl<'a> Pipes<'a> {
    fn new(child: &mut Child, input: &'a [u8]) -> io::Result<Self> {
        let stdin = child.stdin.take().filter(|_| !input.is_empty());
        if let Some(stdin) = &stdin {
            set_nonblocking(stdin)?;
        }
        Ok(Self {
            stdin,
            input,
            stdout: Output::new(child.stdout.take())?,
            stderr: Output::new(child.stderr.take())?,
        })
    }

    fn pump(
        &mut self,
        exit: &PipeReader,
        group: &Group,
        timeout: Option<Duration>,
        kill_on_exit: bool,
    ) -> io::Result<(bool, bool)> {
        let mut deadline = timeout.map(|timeout| Instant::now() + timeout);
        let (mut exited, mut timed_out) = (false, false);
        loop {
            if exited && self.stdout.pipe.is_none() && self.stderr.pipe.is_none() {
                return Ok((timed_out, true));
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                if exited {
                    return Ok((timed_out, false));
                }
                group.kill();
                timed_out = true;
                deadline = None;
            }
            let mut fds = Vec::with_capacity(4);
            if !exited {
                fds.push(watch(exit, libc::POLLIN));
            }
            if let Some(pipe) = &self.stdout.pipe {
                fds.push(watch(pipe, libc::POLLIN));
            }
            if let Some(pipe) = &self.stderr.pipe {
                fds.push(watch(pipe, libc::POLLIN));
            }
            if let Some(pipe) = &self.stdin {
                fds.push(watch(pipe, libc::POLLOUT));
            }
            poll(&mut fds, deadline)?;
            if !exited && fds[0].revents != 0 {
                exited = true;
                if kill_on_exit {
                    group.kill();
                }
                self.stdin = None;
                deadline = Some(Instant::now() + WAIT_DELAY);
            }
            self.stdout.read()?;
            self.stderr.read()?;
            self.feed()?;
        }
    }

    fn feed(&mut self) -> io::Result<()> {
        let Some(stdin) = &mut self.stdin else {
            return Ok(());
        };
        match stdin.write(self.input) {
            Ok(written) => {
                self.input = &self.input[written..];
                if self.input.is_empty() {
                    self.stdin = None;
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                self.stdin = None;
                Ok(())
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

fn watch(fd: &impl AsRawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    }
}

fn poll(fds: &mut [libc::pollfd], deadline: Option<Instant>) -> io::Result<()> {
    loop {
        let timeout = deadline.map_or(-1, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            libc::c_int::try_from(remaining.as_nanos().div_ceil(1_000_000))
                .unwrap_or(libc::c_int::MAX)
        });
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn set_nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    let fd = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
