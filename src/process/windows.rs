use std::fs::File;
use std::io::{self, Read, Write};
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, INFINITE, OpenThread, ResumeThread,
    THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

use super::{Communicated, WAIT_DELAY};

pub fn signal(_: ExitStatus) -> Option<i32> {
    None
}

pub struct Process {
    child: Child,
    group: Arc<Group>,
    stdin: Option<File>,
    stdout: Option<File>,
    stderr: Option<File>,
}

impl Process {
    // The pipes are created here rather than with `Stdio::piped()` so that they are synchronous, which
    // is what lets `CancelSynchronousIo` stop a read or write that a process outside the job blocks.
    pub fn spawn(mut command: Command, piped_stdin: bool) -> io::Result<Self> {
        let (stdout, child_stdout) = pipe()?;
        let (stderr, child_stderr) = pipe()?;
        let stdin = if piped_stdin {
            let (child_stdin, stdin) = pipe()?;
            command.stdin(Stdio::from(child_stdin));
            Some(File::from(stdin))
        } else {
            command.stdin(Stdio::null());
            None
        };
        command
            .stdout(Stdio::from(child_stdout))
            .stderr(Stdio::from(child_stderr))
            .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        drop(command);
        let group = Arc::new(Group::new(&mut child)?);
        Ok(Self {
            child,
            group,
            stdin,
            stdout: Some(File::from(stdout)),
            stderr: Some(File::from(stderr)),
        })
    }

    pub fn group(&self) -> Arc<Group> {
        Arc::clone(&self.group)
    }

    pub fn communicate(
        &mut self,
        input: Option<&str>,
        timeout: Option<Duration>,
    ) -> io::Result<Communicated> {
        let mut stdout = self.stdout.take().expect("communicate runs once");
        let mut stderr = self.stderr.take().expect("communicate runs once");
        let stdout = Worker::spawn(move |stop| read(&mut stdout, stop));
        let stderr = Worker::spawn(move |stop| read(&mut stderr, stop));
        let stdin = self.stdin.take().zip(input).map(|(mut stdin, input)| {
            let input = input.to_owned();
            Worker::spawn(move |stop| write(&mut stdin, &input, stop))
        });
        let timed_out = match wait_for_exit(&self.child, timeout) {
            Ok(true) => Ok(false),
            Ok(false) => {
                self.group.kill();
                wait_for_exit(&self.child, None).map(|_| true)
            }
            Err(error) => Err(error),
        };
        self.group.kill();
        let deadline = Instant::now() + WAIT_DELAY;
        let (stdout, stderr) = (stdout.finish(deadline), stderr.finish(deadline));
        let fed = stdin.map(|stdin| stdin.finish(deadline)).transpose();
        let timed_out = timed_out?;
        let ((stdout, stdout_closed), (stderr, stderr_closed)) = (stdout?, stderr?);
        fed?;
        Ok(Communicated {
            stdout,
            stderr,
            timed_out,
            output_closed: stdout_closed && stderr_closed,
        })
    }

    pub fn wait(mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }
}

struct Worker<T> {
    thread: JoinHandle<io::Result<T>>,
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
}

impl<T: Send + 'static> Worker<T> {
    fn spawn(work: impl FnOnce(&AtomicBool) -> io::Result<T> + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (finished, done) = mpsc::channel();
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let result = work(&flag);
            let _ = finished.send(());
            result
        });
        Self { thread, stop, done }
    }

    fn finish(self, deadline: Instant) -> io::Result<(T, bool)> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let finished = self.done.recv_timeout(remaining) != Err(RecvTimeoutError::Timeout);
        if !finished {
            self.stop.store(true, Ordering::SeqCst);
            // A cancel that lands between two calls is lost, so repeat it until the worker stops.
            loop {
                unsafe {
                    CancelSynchronousIo(self.thread.as_raw_handle());
                }
                if self.done.recv_timeout(Duration::from_millis(10))
                    != Err(RecvTimeoutError::Timeout)
                {
                    break;
                }
            }
        }
        let value = self
            .thread
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
        Ok((value, finished))
    }
}

fn read(pipe: &mut File, stop: &AtomicBool) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0; 8192];
    while !stop.load(Ordering::SeqCst) {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => output.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break,
            Err(_) if stop.load(Ordering::SeqCst) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(output)
}

fn write(pipe: &mut File, input: &str, stop: &AtomicBool) -> io::Result<()> {
    match pipe.write_all(input.as_bytes()) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe || stop.load(Ordering::SeqCst) => {
            Ok(())
        }
        result => result,
    }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (ptr::null_mut(), ptr::null_mut());
    check(unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) })?;
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

fn wait_for_exit(child: &Child, timeout: Option<Duration>) -> io::Result<bool> {
    let milliseconds = timeout.map_or(INFINITE, |timeout| {
        u32::try_from(timeout.as_millis())
            .unwrap_or(INFINITE)
            .min(INFINITE - 1)
    });
    match unsafe { WaitForSingleObject(child.as_raw_handle(), milliseconds) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

pub struct Group {
    job: OwnedHandle,
}

impl Group {
    // The child starts suspended so that it is in the job before it can start any process of its own.
    fn new(child: &mut Child) -> io::Result<Self> {
        let group = Self::assign(child).and_then(|group| {
            resume(child.id())?;
            Ok(group)
        });
        if group.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        group
    }

    fn assign(child: &Child) -> io::Result<Self> {
        let job = owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        check(unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                mem::size_of_val(&limits) as u32,
            )
        })?;
        check(unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) })?;
        Ok(Self { job })
    }

    pub fn kill(&self) {
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
}

fn resume(process_id: u32) -> io::Result<()> {
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry: THREADENTRY32 = unsafe { mem::zeroed() };
    entry.dwSize = mem::size_of::<THREADENTRY32>() as u32;
    check(unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) })?;
    while entry.th32OwnerProcessID != process_id {
        check(unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) })?;
    }
    let thread = owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
    match unsafe { ResumeThread(thread.as_raw_handle()) } {
        u32::MAX => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
