//! OS limits are installed before parsing input, never in the GUI process.
use crate::Result;

#[cfg(unix)]
pub(super) fn install() -> Result<()> {
    // The child has only stdin/stdout/stderr, read-only sources and their maps.
    for (resource, value) in [
        (libc::RLIMIT_CPU, 3),
        (libc::RLIMIT_NOFILE, 64),
        (libc::RLIMIT_CORE, 0),
        (libc::RLIMIT_FSIZE, 0),
    ] {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: valid resource constant and live rlimit; only this child is affected.
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(target_os = "linux")]
    {
        let limit = libc::rlimit {
            rlim_cur: 512 * 1024 * 1024,
            rlim_max: 512 * 1024 * 1024,
        };
        // SAFETY: Linux enforces this address-space limit on this worker only.
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &limit) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    #[cfg(target_os = "linux")]
    // SAFETY: documented prctl operation with no pointer arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Darwin's RLIMIT_AS is not a reliable hard address-space cap. Supervise the
/// child's resident memory instead, every 10 ms in the parent wait loop.
#[cfg(target_os = "macos")]
pub(super) fn check(pid: u32) -> Result<()> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: a live, correctly sized task-info buffer; this only queries our child.
    let got = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut _,
            size,
        )
    };
    if got != size {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(error.into());
    }
    if info.pti_resident_size > 512 * 1024 * 1024 {
        return Err(crate::Error::Limit(
            "preview worker resident memory exceeds 512 MiB".into(),
        ));
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
pub(super) fn check(_pid: u32) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub(super) struct Limits(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Drop for Limits {
    fn drop(&mut self) {
        // SAFETY: this handle belongs to the current worker and is closed once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(windows)]
pub(super) fn install() -> Result<Limits> {
    use windows_sys::Win32::System::{JobObjects::*, Threading::GetCurrentProcess};
    // SAFETY: null attributes/name create a private job; structures are fully initialized.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let job = Limits(handle);
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_PROCESS_TIME;
    info.BasicLimitInformation.PerProcessUserTimeLimit = 30_000_000; // 3 seconds in 100 ns.
    info.ProcessMemoryLimit = 512 * 1024 * 1024;
    if unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        )
    } == 0
        || unsafe { AssignProcessToJobObject(handle, GetCurrentProcess()) } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(job)
}
