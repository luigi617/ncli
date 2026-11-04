// A session id is a process id, which the system reuses once the session ends, so the start time of
// the session leader tells a later session with the same id apart.
#[cfg(unix)]
pub fn id() -> String {
    of(unsafe { libc::getsid(0) })
}

#[cfg(unix)]
fn of(session: libc::pid_t) -> String {
    match started(session) {
        Some(started) => format!("{session}-{started}"),
        None => session.to_string(),
    }
}

// Only a missing process or a different session in its place proves the session ended; a session
// the system will not describe may still be alive.
#[cfg(unix)]
pub fn ended(id: &str) -> bool {
    let session = id.split_once('-').map_or(id, |(session, _)| session);
    let Ok(session) = session.parse::<libc::pid_t>() else {
        return false;
    };
    match unsafe { libc::getsid(session) } {
        -1 => std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        leader => leader != session || of(session) != id,
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn started(process: libc::pid_t) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{process}/stat")).ok()?;
    let (_, fields) = stat.rsplit_once(')')?;
    fields.split_whitespace().nth(19).map(str::to_owned)
}

// The leader is often a root-owned `login`, which `proc_pidinfo` refuses to describe to other users,
// so read `kinfo_proc`, whose first field is `p_starttime`, the way `ps` does.
#[cfg(target_os = "macos")]
fn started(process: libc::pid_t) -> Option<String> {
    let mut name = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        process,
    ];
    let mut length = 0;
    let mut query = |buffer: *mut libc::c_void, length: &mut usize| unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name.len() as libc::c_uint,
            buffer,
            length,
            std::ptr::null_mut(),
            0,
        ) == 0
    };
    query(std::ptr::null_mut(), &mut length).then_some(())?;
    let mut buffer = vec![0u8; length];
    query(buffer.as_mut_ptr().cast(), &mut length).then_some(())?;
    (length >= size_of::<libc::timeval>()).then(|| {
        let started = unsafe { buffer.as_ptr().cast::<libc::timeval>().read_unaligned() };
        format!("{}.{:06}", started.tv_sec, started.tv_usec)
    })
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
fn started(_: libc::pid_t) -> Option<String> {
    None
}

#[cfg(windows)]
pub fn id() -> String {
    let console = unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() };
    format!("{:x}", console as usize)
}

#[cfg(windows)]
pub fn ended(id: &str) -> bool {
    usize::from_str_radix(id, 16).is_ok_and(|console| unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(console as _) == 0
    })
}
