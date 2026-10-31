use std::io;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus};
use std::ptr;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, INFINITE, OpenThread, ResumeThread,
    THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

pub fn configure(command: &mut Command) {
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
}

pub fn signal(_: ExitStatus) -> Option<i32> {
    None
}

pub struct Group {
    job: OwnedHandle,
}

impl Group {
    // The child starts suspended so that it is in the job before it can start any process of its own.
    pub fn new(child: &mut Child) -> io::Result<Self> {
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

pub fn wait_for_exit(child: &Child) -> io::Result<()> {
    match unsafe { WaitForSingleObject(child.as_raw_handle(), INFINITE) } {
        WAIT_OBJECT_0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
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
