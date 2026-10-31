use std::io::{self, PipeReader, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{Communicated, WAIT_DELAY};

pub struct Process {
    child: Child,
    group: Arc<Group>,
}

impl Process {
    pub fn spawn(mut command: Command, piped_stdin: bool) -> io::Result<Self> {
        command
            .stdin(if piped_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command.spawn()?;
        let group = Arc::new(Group::new(&child)?);
        Ok(Self { child, group })
    }

    pub fn group(&self) -> Arc<Group> {
        Arc::clone(&self.group)
    }

    pub fn communicate(
        &mut self,
        input: Option<&str>,
        timeout: Option<Duration>,
    ) -> io::Result<Communicated> {
        communicate(&mut self.child, &self.group, input, timeout)
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
}

impl Group {
    fn new(child: &Child) -> io::Result<Self> {
        let id = libc::pid_t::try_from(child.id()).map_err(io::Error::other)?;
        Ok(Self { id })
    }

    // Only valid while the leader is unreaped: until then the kernel cannot reuse the group id.
    pub fn kill(&self) {
        unsafe {
            libc::killpg(self.id, libc::SIGKILL);
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

fn communicate(
    child: &mut Child,
    group: &Group,
    input: Option<&str>,
    timeout: Option<Duration>,
) -> io::Result<Communicated> {
    let setup = Pipes::new(child, input.unwrap_or_default().as_bytes())
        .and_then(|pipes| Ok((pipes, io::pipe()?)));
    let (mut pipes, (exit_reader, exit_writer)) = setup.inspect_err(|_| group.kill())?;
    let child = &*child;
    thread::scope(|scope| {
        let waiter = scope.spawn(move || {
            let result = wait_for_exit(child);
            drop(exit_writer);
            result
        });
        let pumped = pipes.pump(&exit_reader, group, timeout);
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

struct Output<P> {
    pipe: Option<P>,
    output: Vec<u8>,
}

impl<P: Read + AsRawFd> Output<P> {
    fn new(pipe: P) -> io::Result<Self> {
        set_nonblocking(&pipe)?;
        Ok(Self {
            pipe: Some(pipe),
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
            stdout: Output::new(child.stdout.take().expect("stdout is piped"))?,
            stderr: Output::new(child.stderr.take().expect("stderr is piped"))?,
        })
    }

    fn pump(
        &mut self,
        exit: &PipeReader,
        group: &Group,
        timeout: Option<Duration>,
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
            let ready = |fd: RawFd| {
                fds.iter()
                    .any(|polled| polled.fd == fd && polled.revents != 0)
            };
            if !exited && ready(exit.as_raw_fd()) {
                exited = true;
                group.kill();
                self.stdin = None;
                deadline = Some(Instant::now() + WAIT_DELAY);
            }
            if self
                .stdout
                .pipe
                .as_ref()
                .is_some_and(|pipe| ready(pipe.as_raw_fd()))
            {
                self.stdout.read()?;
            }
            if self
                .stderr
                .pipe
                .as_ref()
                .is_some_and(|pipe| ready(pipe.as_raw_fd()))
            {
                self.stderr.read()?;
            }
            if self
                .stdin
                .as_ref()
                .is_some_and(|pipe| ready(pipe.as_raw_fd()))
            {
                self.feed()?;
            }
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
