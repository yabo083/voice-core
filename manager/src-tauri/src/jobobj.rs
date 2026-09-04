//! Kernel-enforced ownership of a spawned process tree.
//!
//! Killing a PowerShell process by PID leaves its children — git, uv, pip,
//! python, a half-finished multi-gigabyte download — running with their pipes
//! closed and the bytes still flowing. A job object with KILL_ON_JOB_CLOSE
//! removes the question: every process in the job dies when the handle closes,
//! including when this GUI is killed rather than asked to quit, and no PID is
//! ever targeted by number. It is the same mechanism the runtime uses to own its
//! engine worker.

pub use imp::Job;

/// Kill a process tree by walking parent links, as a belt to the job object's
/// braces.
///
/// There is a window between `spawn` and `assign` in which the child can start a
/// grandchild, and a grandchild born in that window is outside the job for good:
/// assigning the parent afterwards does not adopt children it already has. This
/// catches exactly those escapees — but only while the tree still exists, which
/// is why callers run it *before* dropping the job.
pub fn kill_tree(pid: u32) {
    let mut command = std::process::Command::new("taskkill");
    command
        .arg("/T")
        .arg("/F")
        .arg("/PID")
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::host::hidden(&mut command);
    // A failure here means the tree is already gone, which is the goal.
    let _ = command.status();
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Dropping this kills everything assigned to it.
    pub struct Job(HANDLE);

    impl Job {
        pub fn new() -> io::Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(err);
                }
                Ok(Job(handle))
            }
        }

        /// Assign immediately after spawning. Nested jobs are supported since
        /// Windows 8, so being inside someone else's job is not a problem.
        pub fn assign(&self, child: &Child) -> io::Result<()> {
            unsafe {
                if AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    // The handle is owned exclusively by this struct and only ever used through
    // it, so moving it between threads is sound even though a raw HANDLE is not
    // `Send` by default.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
}

#[cfg(not(windows))]
mod imp {
    use std::io;
    use std::process::Child;

    /// There is no portable equivalent, so this reports the truth instead of
    /// pretending: callers log it and fall back to killing the direct child,
    /// which leaves grandchildren behind. The install this app ships in is
    /// Windows-only, so the fallback is never taken in practice.
    pub struct Job;

    impl Job {
        pub fn new() -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process-tree ownership requires a Win32 job object",
            ))
        }

        pub fn assign(&self, _child: &Child) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "process-tree ownership requires a Win32 job object",
            ))
        }
    }
}
