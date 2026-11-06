//! Own the remote bootstrap in a disposable process, never in the GUI/CLI.
//! The GUI may duplicate from its ordinary child; it must never receive
//! PROCESS_DUP_HANDLE on the elevated helper (that would permit escalation).
use super::*;
use std::{
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
};
use windows_sys::Win32::System::JobObjects::*;

pub(super) const FLAG: &str = "--rescue-read-receiver";

struct OwnedReceiver {
    child: Child,
    job: Handle,
}
impl OwnedReceiver {
    fn spawn(command: &mut Command) -> Result<Self> {
        let job = Handle::new(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(os_error());
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let owned = Self { child, job };
        // The child cannot request elevation before this assignment: it must
        // first receive the authenticated request, which we have not sent yet.
        // If this parent dies between spawn and assignment, its pipe closes and
        // the child exits without opening a device or launching a helper.
        if unsafe { AssignProcessToJobObject(owned.job.0, owned.child.as_raw_handle()) } == 0 {
            return Err(os_error());
        }
        Ok(owned)
    }
}
impl Drop for OwnedReceiver {
    fn drop(&mut self) {
        // No external process is assigned to this unnamed, non-inheritable job.
        // Closing it also covers termination of the GUI/CLI itself.
        unsafe {
            TerminateJobObject(self.job.0, 125);
        }
        let _ = self.child.kill();
        // Never block a GUI cancellation indefinitely on a broken kernel I/O.
        unsafe {
            WaitForSingleObject(self.child.as_raw_handle(), 1000);
        }
    }
}

fn accept(pipe: &mut Pipe, child: &Child, cancel: &AtomicBool) -> Result<()> {
    loop {
        let ok = unsafe { ConnectNamedPipe(pipe.file.as_raw_handle(), std::ptr::null_mut()) };
        let error = std::io::Error::last_os_error();
        if ok != 0 || error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
            break;
        }
        if !matches!(error.raw_os_error(), Some(x) if x == ERROR_PIPE_LISTENING as i32 || x == ERROR_NO_DATA as i32)
        {
            return Err(error.into());
        }
        if unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
            return Err(Error::Request(
                "read receiver exited before connecting".into(),
            ));
        }
        wait(pipe.deadline, cancel)?;
    }
    verify_client(pipe, child.id())?;
    same_executable(child.as_raw_handle())
}

pub(super) fn request(
    source: &Path,
    cancel: &AtomicBool,
) -> Result<(File, crate::device::DeviceInfo)> {
    let nonce = Uuid::new_v4().to_string();
    let mut pipe = server(&nonce)?;
    let mut command = Command::new(std::env::current_exe()?);
    command.args([
        FLAG,
        &nonce,
        &std::process::id().to_string(),
        &creation(unsafe { GetCurrentProcess() })?.to_string(),
    ]);
    let receiver = OwnedReceiver::spawn(&mut command)?;
    accept(&mut pipe, &receiver.child, cancel)?;
    let ready: Ack = pipe.receive(cancel)?;
    if ready.nonce != nonce {
        return Err(Error::Request("read receiver readiness mismatch".into()));
    }
    pipe.send(
        &Request {
            nonce: nonce.clone(),
            source: source.to_owned(),
        },
        cancel,
    )?;
    // Only user consent is unbounded. The receiver starts a fresh 30-second
    // IPC deadline after ShellExecuteExW returns, including on UAC denial.
    // Cancellation closes this pipe and kills our own ordinary receiver.
    pipe.deadline = None;
    let response: Response = pipe.receive(cancel)?;
    pipe.deadline = Some(Instant::now() + Duration::from_secs(30));
    if response.nonce != nonce {
        return Err(Error::Request("read receiver nonce mismatch".into()));
    }
    if response.cancelled {
        return Err(Error::Cancelled);
    }
    if let Some(error) = response.error {
        return Err(Error::Request(format!("read receiver: {error}")));
    }
    let remote = usize::try_from(response.handle)
        .map_err(|_| Error::Request("read receiver handle width".into()))?
        as HANDLE;
    // This call creates a locally owned handle. No other process ever writes an
    // unacknowledged handle into the long-lived application handle table.
    let file = local_file(duplicate(
        receiver.child.as_raw_handle(),
        remote,
        unsafe { GetCurrentProcess() },
        true,
    )?)?;
    let info = crate::device::device_info(&file, source)?;
    if info.bytes != response.bytes
        || info.sector != response.sector
        || info.identity != response.identity
    {
        return Err(Error::SourceChanged);
    }
    pipe.send(&Ack { nonce }, cancel)?;
    while unsafe { WaitForSingleObject(receiver.child.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
        wait(pipe.deadline, cancel)?;
    }
    let mut exit = 4;
    if unsafe { GetExitCodeProcess(receiver.child.as_raw_handle(), &mut exit) } == 0 || exit != 0 {
        return Err(Error::Request(
            "read receiver did not finish its handoff".into(),
        ));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok((file, info))
}

pub(super) fn run(nonce: &str, parent_pid: u32, parent_created: u64) -> Result<()> {
    let (_parent, mut pipe) = connect_parent(nonce, parent_pid, parent_created, false)?;
    let cancel = AtomicBool::new(false);
    pipe.send(
        &Ack {
            nonce: nonce.into(),
        },
        &cancel,
    )?;
    let request: Request = pipe.receive(&cancel)?;
    if request.nonce != nonce {
        return Err(Error::Request("read receiver request mismatch".into()));
    }
    let source = source_path(&request.source)?;
    let operation = receive_elevated(&source, &cancel);
    pipe.deadline = Some(Instant::now() + Duration::from_secs(30));
    let (file, info) = match operation {
        Ok(value) => value,
        Err(error) => {
            pipe.send(
                &Response {
                    nonce: nonce.into(),
                    handle: 0,
                    bytes: 0,
                    sector: 0,
                    identity: String::new(),
                    error: Some(error.to_string()),
                    cancelled: matches!(error, Error::Cancelled),
                },
                &cancel,
            )?;
            return Ok(());
        }
    };
    // The helper has exited, and this File remains alive until the parent has
    // duplicated it. A killed receiver simply causes duplication to fail.
    pipe.send(
        &Response {
            nonce: nonce.into(),
            handle: file.as_raw_handle() as usize as u64,
            bytes: info.bytes,
            sector: info.sector,
            identity: info.identity,
            error: None,
            cancelled: false,
        },
        &cancel,
    )?;
    let ack: Ack = pipe.receive(&cancel)?;
    if ack.nonce != nonce {
        return Err(Error::Request(
            "read receiver acknowledgement mismatch".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const CHILD_TEST: &str = "read_access::windows::receiver::tests::owned_fixture_child";

    #[derive(Serialize, Deserialize)]
    struct Fixture {
        nonce: String,
        parent: u32,
        created: u64,
        role: String,
        path: PathBuf,
        target: u32,
    }
    fn spawn_fixture(role: &str, path: &Path, target: u32) -> (OwnedReceiver, Pipe) {
        let nonce = Uuid::new_v4().to_string();
        let mut pipe = server(&nonce).unwrap();
        let fixture = Fixture {
            nonce,
            parent: std::process::id(),
            created: creation(unsafe { GetCurrentProcess() }).unwrap(),
            role: role.into(),
            path: path.into(),
            target,
        };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
            .env(
                "RESCUE_OWNED_RECEIVER_TEST",
                serde_json::to_string(&fixture).unwrap(),
            );
        let owned = OwnedReceiver::spawn(&mut command).unwrap();
        accept(&mut pipe, &owned.child, &AtomicBool::new(false)).unwrap();
        let ready: Ack = pipe.receive(&AtomicBool::new(false)).unwrap();
        assert_eq!(ready.nonce, fixture.nonce);
        (owned, pipe)
    }

    // This entry exists only in the Rust test binary. Production binaries have
    // no environment-controlled crash, ordinary-file, or authentication bypass.
    #[test]
    #[ignore = "owned subprocess entry, launched by the tests below"]
    fn owned_fixture_child() {
        let fixture: Fixture =
            serde_json::from_str(&std::env::var("RESCUE_OWNED_RECEIVER_TEST").unwrap()).unwrap();
        let (_parent, mut pipe) =
            connect_parent(&fixture.nonce, fixture.parent, fixture.created, false).unwrap();
        let cancel = AtomicBool::new(false);
        pipe.send(
            &Ack {
                nonce: fixture.nonce.clone(),
            },
            &cancel,
        )
        .unwrap();
        match fixture.role.as_str() {
            "receiver" => {
                let _: Ack = pipe.receive(&cancel).unwrap();
            }
            "donor" => {
                let target = Handle::new(unsafe {
                    OpenProcess(
                        PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION,
                        0,
                        fixture.target,
                    )
                })
                .unwrap();
                same_executable(target.0).unwrap();
                let file = OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(&fixture.path)
                    .unwrap();
                let _remote = duplicate(
                    unsafe { GetCurrentProcess() },
                    file.as_raw_handle(),
                    target.0,
                    true,
                )
                .unwrap();
                // Only this test observer gets the marker. The receiver never
                // learns the remote value, matching death before the response.
                pipe.send(
                    &Ack {
                        nonce: "duplicated".into(),
                    },
                    &cancel,
                )
                .unwrap();
                let _: Ack = pipe.receive(&cancel).unwrap();
            }
            "owner" => {
                let (child, _child_pipe) = spawn_fixture("receiver", &fixture.path, 0);
                let file = OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(&fixture.path)
                    .unwrap();
                duplicate(
                    unsafe { GetCurrentProcess() },
                    file.as_raw_handle(),
                    child.child.as_raw_handle(),
                    true,
                )
                .unwrap();
                drop(file);
                pipe.send(&child.child.id(), &cancel).unwrap();
                let _: Ack = pipe.receive(&cancel).unwrap();
            }
            other => panic!("unknown test role {other}"),
        }
    }

    #[test]
    fn killed_donor_leaves_no_handle_after_disposable_receiver_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-bootstrap.bin");
        std::fs::write(&path, b"test-owned read-only transfer").unwrap();
        let (receiver, _receiver_pipe) = spawn_fixture("receiver", &path, 0);
        let (donor, mut donor_pipe) = spawn_fixture("donor", &path, receiver.child.id());
        let marker: Ack = donor_pipe.receive(&AtomicBool::new(false)).unwrap();
        assert_eq!(marker.nonce, "duplicated");
        // TerminateProcess skips Bootstrap::drop. The receiver's unclaimed
        // remote handle keeps the file locked after the donor is definitely dead.
        assert_ne!(
            unsafe { TerminateProcess(donor.child.as_raw_handle(), 126) },
            0
        );
        assert_eq!(
            unsafe { WaitForSingleObject(donor.child.as_raw_handle(), 2000) },
            WAIT_OBJECT_0
        );
        assert!(std::fs::remove_file(&path).is_err());
        let started = Instant::now();
        drop(receiver);
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn killed_owner_closes_the_job_and_reclaims_unacknowledged_handles() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-parent-crash.bin");
        std::fs::write(&path, b"parent-death test only").unwrap();
        let (owner, mut pipe) = spawn_fixture("owner", &path, 0);
        let receiver_pid: u32 = pipe.receive(&AtomicBool::new(false)).unwrap();
        let receiver = Handle::new(unsafe { OpenProcess(SYNCHRONIZE, 0, receiver_pid) }).unwrap();
        assert!(std::fs::remove_file(&path).is_err());
        // Kill just the owner, not its outer test job. Only the owner's private
        // KILL_ON_JOB_CLOSE job can now terminate the receiver descendant.
        assert_ne!(
            unsafe { TerminateProcess(owner.child.as_raw_handle(), 127) },
            0
        );
        assert_eq!(
            unsafe { WaitForSingleObject(owner.child.as_raw_handle(), 2000) },
            WAIT_OBJECT_0
        );
        assert_eq!(
            unsafe { WaitForSingleObject(receiver.0, 2000) },
            WAIT_OBJECT_0
        );
        std::fs::remove_file(&path).unwrap();
    }
}
