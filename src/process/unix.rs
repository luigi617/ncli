use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus};

pub fn configure(command: &mut Command) {
    command.process_group(0);
}

pub fn signal(status: ExitStatus) -> Option<i32> {
    status.signal()
}

pub struct Group {
    id: libc::pid_t,
}

impl Group {
    pub fn new(child: &mut Child) -> io::Result<Self> {
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

pub fn wait_for_exit(child: &Child) -> io::Result<()> {
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
