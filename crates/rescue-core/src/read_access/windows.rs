//! One-shot, authenticated handle handoff. The elevated helper opens one strict
//! disk/volume path read-only and exits; no parser or output writer is elevated.
use super::*;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Com::*, Pipes::*, RemoteDesktop::ProcessIdToSessionId, Threading::*},
    UI::{Shell::*, WindowsAndMessaging::SW_HIDE},
};

const FLAG: &str = "--rescue-read-broker";
const FRAME: usize = 4096;
static GRANTS: OnceLock<Mutex<BTreeMap<PathBuf, File>>> = OnceLock::new();
fn grants() -> &'static Mutex<BTreeMap<PathBuf, File>> {
    GRANTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
fn os_error() -> Error {
    std::io::Error::last_os_error().into()
}
struct Handle(HANDLE);
impl Handle {
    fn new(value: HANDLE) -> Result<Self> {
        if value.is_null() || value == INVALID_HANDLE_VALUE {
            Err(os_error())
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Local(*mut std::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Com;
impl Com {
    fn new() -> Result<Self> {
        // SAFETY: called on this fresh worker thread; balanced at thread return.
        let result = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        if result < 0 {
            return Err(Error::Request(format!("COM initialization: {result:#x}")));
        }
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

fn creation(process: HANDLE) -> Result<u64> {
    let mut start = FILETIME::default();
    let mut end = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: valid process and four live writable FILETIME outputs.
    if unsafe { GetProcessTimes(process, &mut start, &mut end, &mut kernel, &mut user) } == 0 {
        return Err(os_error());
    }
    Ok((u64::from(start.dwHighDateTime) << 32) | u64::from(start.dwLowDateTime))
}
fn image_path(process: HANDLE) -> Result<PathBuf> {
    let mut path = vec![0u16; 32768];
    let mut len = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) } == 0 {
        return Err(os_error());
    }
    use std::os::windows::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &path[..len as usize],
    )))
}
fn same_executable(process: HANDLE) -> Result<()> {
    let peer = File::open(image_path(process)?)?;
    let own = File::open(std::env::current_exe()?)?;
    if same_file::Handle::from_file(peer)? != same_file::Handle::from_file(own)? {
        return Err(Error::Request(
            "read broker requires the same executable file".into(),
        ));
    }
    Ok(())
}
fn session(pid: u32) -> Result<u32> {
    let mut id = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut id) } == 0 {
        return Err(os_error());
    }
    Ok(id)
}
fn token(process: HANDLE) -> Result<Handle> {
    let mut handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut handle) } == 0 {
        return Err(os_error());
    }
    Handle::new(handle)
}
fn user_sid() -> Result<String> {
    let token = token(unsafe { GetCurrentProcess() })?;
    let mut count = 0;
    unsafe {
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut count);
    }
    if count == 0 || count > 65536 {
        return Err(Error::Request("token user size".into()));
    }
    let mut bytes = vec![0u64; (count as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            bytes.as_mut_ptr().cast(),
            count,
            &mut count,
        )
    } == 0
    {
        return Err(os_error());
    }
    // SAFETY: the aligned buffer was filled with TOKEN_USER by Windows.
    let user = unsafe { &*bytes.as_ptr().cast::<TOKEN_USER>() };
    let mut pointer = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut pointer) } == 0 {
        return Err(os_error());
    }
    let allocated = Local(pointer.cast());
    let mut length = 0;
    while length < 256 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    if length == 256 {
        return Err(Error::Request("SID string length".into()));
    }
    let value = String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, length) })
        .map_err(|_| Error::Invalid("SID text".into()))?;
    drop(allocated);
    Ok(value)
}
fn elevated() -> Result<bool> {
    let token = token(unsafe { GetCurrentProcess() })?;
    let mut result = TOKEN_ELEVATION::default();
    let mut count = 0;
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut result as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of_val(&result) as u32,
            &mut count,
        )
    } == 0
    {
        return Err(os_error());
    }
    Ok(result.TokenIsElevated != 0)
}
fn source_path(path: &Path) -> Result<PathBuf> {
    if !crate::device::is_device(path) {
        return Err(Error::Request(
            "read broker accepts only explicit Windows disk/volume paths".into(),
        ));
    }
    let path = crate::device::canonical_source_path(path)?;
    crate::source_scope::check(&path)?;
    Ok(path)
}
pub(crate) fn granted(path: &Path) -> std::io::Result<Option<File>> {
    grants()
        .lock()
        .map_err(|_| std::io::Error::other("read permission registry"))?
        .get(path)
        .map(File::try_clone)
        .transpose()
}
fn remember(path: PathBuf, file: File) -> Result<()> {
    let mut grants = grants()
        .lock()
        .map_err(|_| Error::Request("read permission registry".into()))?;
    if grants.len() >= 16 && !grants.contains_key(&path) {
        return Err(Error::Limit("16 read-permitted devices per process".into()));
    }
    grants.insert(path, file);
    Ok(())
}

struct Pipe {
    file: File,
    deadline: Instant,
}
fn wait(deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Error::Request("read broker IPC timed out".into()));
    }
    std::thread::sleep(Duration::from_millis(10));
    Ok(())
}
impl Pipe {
    fn send(&mut self, value: &impl Serialize, cancel: &AtomicBool) -> Result<()> {
        let payload = serde_json::to_vec(value)?;
        if payload.len() > FRAME {
            return Err(Error::Limit("read broker frame".into()));
        }
        let bytes = [
            (payload.len() as u32).to_le_bytes().as_slice(),
            payload.as_slice(),
        ]
        .concat();
        let mut at = 0;
        while at < bytes.len() {
            match self.file.write(&bytes[at..]) {
                Ok(n) if n > 0 => at += n,
                Ok(_) => wait(self.deadline, cancel)?,
                Err(e) if matches!(e.raw_os_error(),Some(x) if x==ERROR_NO_DATA as i32||x==ERROR_PIPE_BUSY as i32) => {
                    wait(self.deadline, cancel)?
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
    fn exact(&mut self, bytes: &mut [u8], cancel: &AtomicBool) -> Result<()> {
        let mut at = 0;
        while at < bytes.len() {
            // std::fs::File maps ERROR_NO_DATA to EOF on Windows. A nonblocking
            // pipe without a frame yet must instead wait; preserve ReadFile's
            // native error so absence of data cannot look like a disconnected peer.
            let mut read = 0;
            let ok = unsafe {
                ReadFile(
                    self.file.as_raw_handle(),
                    bytes[at..].as_mut_ptr(),
                    (bytes.len() - at) as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok != 0 {
                if read == 0 {
                    wait(self.deadline, cancel)?;
                } else {
                    at += read as usize;
                }
            } else {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NO_DATA as i32) {
                    wait(self.deadline, cancel)?;
                } else {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }
    fn receive<T: for<'de> Deserialize<'de>>(&mut self, cancel: &AtomicBool) -> Result<T> {
        let mut header = [0; 4];
        self.exact(&mut header, cancel)?;
        let length = u32::from_le_bytes(header) as usize;
        if length == 0 || length > FRAME {
            return Err(Error::Limit("read broker frame".into()));
        }
        let mut bytes = vec![0; length];
        self.exact(&mut bytes, cancel)?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}
fn pipe_name(nonce: &str) -> String {
    format!(r"\\.\pipe\RescueRead-{nonce}")
}
thread_local! { static HELPER_STAGE: std::cell::Cell<u32> = const { std::cell::Cell::new(1) }; }
fn setup_stage(stage: u32) {
    HELPER_STAGE.with(|s| s.set(stage));
}
fn helper_exit_error(code: u32) -> Error {
    if code & 0xf000_0000 == 0xa000_0000 {
        let stage = match (code >> 24) & 15 {
            1 => "arguments",
            2 => "elevation",
            3 => "session",
            4 => "open_parent",
            5 => "parent_creation",
            6 => "same_executable",
            7 => "open_pipe",
            8 => "pipe_mode",
            9 => "pipe_peer",
            _ => "handoff",
        };
        Error::Request(format!(
            "read broker setup failed at {stage}; Windows error {}",
            code & 0x00ff_ffff
        ))
    } else {
        Error::Request(format!(
            "read broker exited before connecting; exit code {code:#010x}"
        ))
    }
}
fn server(nonce: &str) -> Result<Pipe> {
    // BA permits an explicitly chosen administrator credential at the UAC prompt.
    // The exact returned helper PID is checked before any request is sent.
    let sddl = wide(format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{})",
        user_sid()?
    ));
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error());
    }
    let _descriptor = Local(descriptor);
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let name = wide(pipe_name(nonce));
    // SAFETY: valid descriptor and UTF-16 path. One local nonblocking instance.
    let handle = Handle::new(unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            8192,
            8192,
            0,
            &security,
        )
    })?;
    let raw = handle.0;
    std::mem::forget(handle);
    Ok(Pipe {
        file: unsafe { File::from_raw_handle(raw) },
        deadline: Instant::now() + Duration::from_secs(30),
    })
}
#[derive(Serialize, Deserialize)]
struct Request {
    nonce: String,
    source: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Response {
    nonce: String,
    handle: u64,
    bytes: u64,
    sector: u32,
    identity: String,
    error: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Ack {
    nonce: String,
}

/// The helper alone owns this bootstrap handle in the parent. The parent makes
/// its own read-only duplicate, then acknowledges. Every helper exit revokes the
/// bootstrap, including broken pipes/cancellation, without double-closing a
/// handle owned by the parent application.
struct Bootstrap {
    process: HANDLE,
    handle: HANDLE,
}
impl Drop for Bootstrap {
    fn drop(&mut self) {
        let mut local = std::ptr::null_mut();
        // SAFETY: pinned live parent handle; bootstrap was never owned/closed by it.
        if unsafe {
            DuplicateHandle(
                self.process,
                self.handle,
                GetCurrentProcess(),
                &mut local,
                0,
                0,
                DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
            )
        } != 0
        {
            unsafe {
                CloseHandle(local);
            }
        }
    }
}
fn duplicate(
    source_process: HANDLE,
    source: HANDLE,
    target: HANDLE,
    read_only: bool,
) -> Result<HANDLE> {
    let mut result = std::ptr::null_mut();
    let access = if read_only { GENERIC_READ } else { 0 };
    let flags = if read_only { 0 } else { DUPLICATE_SAME_ACCESS };
    // SAFETY: all three inputs are live kernel handles; no handle is inheritable.
    if unsafe {
        DuplicateHandle(
            source_process,
            source,
            target,
            &mut result,
            access,
            0,
            flags,
        )
    } == 0
    {
        return Err(os_error());
    }
    Ok(result)
}
fn local_file(handle: HANDLE) -> Result<File> {
    let owner = Handle::new(handle)?;
    if unsafe { GetFileType(owner.0) } != FILE_TYPE_DISK {
        return Err(Error::Request("broker returned a non-disk handle".into()));
    }
    read_rights(owner.0)?;
    let raw = owner.0;
    std::mem::forget(owner);
    Ok(unsafe { File::from_raw_handle(raw) })
}
fn read_rights(handle: HANDLE) -> Result<u32> {
    #[repr(C)]
    struct Basic {
        attributes: u32,
        access: u32,
        handles: u32,
        pointers: u32,
        reserved: [u32; 10],
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryObject(
            handle: HANDLE,
            class: u32,
            info: *mut std::ffi::c_void,
            size: u32,
            returned: *mut u32,
        ) -> i32;
    }
    let mut basic = Basic {
        attributes: 0,
        access: 0,
        handles: 0,
        pointers: 0,
        reserved: [0; 10],
    };
    let mut returned = 0;
    // SAFETY: PUBLIC_OBJECT_BASIC_INFORMATION is the documented 56-byte class 0
    // result; this query never reads disk contents or changes access rights.
    let status = unsafe {
        NtQueryObject(
            handle,
            0,
            (&mut basic as *mut Basic).cast(),
            std::mem::size_of_val(&basic) as u32,
            &mut returned,
        )
    };
    let write = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | FILE_DELETE_CHILD
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER;
    if status < 0 || basic.access & write != 0 || basic.access & FILE_READ_DATA == 0 {
        return Err(Error::Request(
            "broker handle must permit reading and deny all write/delete/ACL rights".into(),
        ));
    }
    Ok(basic.access)
}

pub(super) fn request(path: &Path, cancel: &AtomicBool) -> Result<ReadAccess> {
    let source = source_path(path)?;
    if let Some(file) = granted(&source)? {
        let info = crate::device::device_info(&file, &source)?;
        return Ok(ReadAccess {
            source,
            bytes: info.bytes,
            sector_size: info.sector,
            read_only: true,
            granted_access: read_rights(file.as_raw_handle())?,
            parent_elevated: elevated()?,
            helper_exited: true,
        });
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let _com = Com::new()?;
    let nonce = Uuid::new_v4().to_string();
    let mut pipe = server(&nonce)?;
    let parent = std::process::id();
    let created = creation(unsafe { GetCurrentProcess() })?;
    let executable = wide(std::env::current_exe()?);
    let verb = wide("runas");
    let arguments = wide(format!("{FLAG} {nonce} {parent} {created}"));
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: executable.as_ptr(),
        lpParameters: arguments.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // This is the only elevation call, reached only by an explicit GUI/CLI action.
    if unsafe { ShellExecuteExW(&mut execute) } == 0 {
        let error = std::io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(ERROR_CANCELLED as i32) {
            Error::Cancelled
        } else {
            error.into()
        });
    }
    let helper = Handle::new(execute.hProcess)?;
    let helper_pid = unsafe { GetProcessId(helper.0) };
    if helper_pid == 0 {
        return Err(os_error());
    }
    // UAC consent can take arbitrarily long; IPC's deadline begins afterwards.
    pipe.deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ok = unsafe { ConnectNamedPipe(pipe.file.as_raw_handle(), std::ptr::null_mut()) };
        let error = std::io::Error::last_os_error();
        if ok != 0 || error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
            break;
        }
        if !matches!(error.raw_os_error(),Some(x) if x==ERROR_PIPE_LISTENING as i32||x==ERROR_NO_DATA as i32)
        {
            return Err(error.into());
        }
        if unsafe { WaitForSingleObject(helper.0, 0) } == WAIT_OBJECT_0 {
            let mut code = 4;
            if unsafe { GetExitCodeProcess(helper.0, &mut code) } == 0 {
                return Err(os_error());
            }
            return Err(helper_exit_error(code));
        }
        wait(pipe.deadline, cancel)?;
    }
    let mut peer = 0;
    if unsafe { GetNamedPipeClientProcessId(pipe.file.as_raw_handle(), &mut peer) } == 0
        || peer != helper_pid
    {
        return Err(Error::Request("read broker client PID mismatch".into()));
    }
    same_executable(helper.0)?;
    pipe.send(
        &Request {
            nonce: nonce.clone(),
            source: source.clone(),
        },
        cancel,
    )?;
    let response: Response = pipe.receive(cancel)?;
    if response.nonce != nonce {
        return Err(Error::Request("read broker nonce mismatch".into()));
    }
    if let Some(error) = response.error {
        return Err(Error::Request(format!("read broker: {error}")));
    }
    let bootstrap = usize::try_from(response.handle)
        .map_err(|_| Error::Request("read broker handle width".into()))?
        as HANDLE;
    let file = local_file(duplicate(
        unsafe { GetCurrentProcess() },
        bootstrap,
        unsafe { GetCurrentProcess() },
        true,
    )?)?;
    let info = crate::device::device_info(&file, &source)?;
    if info.bytes != response.bytes
        || info.sector != response.sector
        || info.identity != response.identity
    {
        return Err(Error::SourceChanged);
    }
    pipe.send(&Ack { nonce }, cancel)?;
    while unsafe { WaitForSingleObject(helper.0, 0) } != WAIT_OBJECT_0 {
        wait(pipe.deadline, cancel)?;
    }
    let mut exit = 4;
    if unsafe { GetExitCodeProcess(helper.0, &mut exit) } == 0 || exit != 0 {
        return Err(Error::Request(
            "read broker did not finish its handoff".into(),
        ));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let granted_access = read_rights(file.as_raw_handle())?;
    remember(source.clone(), file)?;
    Ok(ReadAccess {
        source,
        bytes: info.bytes,
        sector_size: info.sector,
        read_only: true,
        granted_access,
        parent_elevated: elevated()?,
        helper_exited: true,
    })
}

fn helper(nonce: &str, parent_pid: u32, parent_created: u64) -> Result<()> {
    setup_stage(2);
    if Uuid::parse_str(nonce).is_err() || !elevated()? {
        return Err(Error::Request(
            "read broker requires explicit elevation".into(),
        ));
    }
    setup_stage(3);
    if session(std::process::id())? != session(parent_pid)? {
        return Err(Error::Request("read broker session mismatch".into()));
    }
    setup_stage(4);
    let parent = Handle::new(unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_DUP_HANDLE,
            0,
            parent_pid,
        )
    })?;
    setup_stage(5);
    if creation(parent.0)? != parent_created {
        return Err(Error::Request("read broker parent identity changed".into()));
    }
    setup_stage(6);
    same_executable(parent.0)?;
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    setup_stage(7);
    let file = loop {
        // Identification-only SQOS prevents the unelevated pipe server from
        // impersonating this elevated helper's token.
        match OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(pipe_name(nonce))
        {
            Ok(file) => break file,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                wait(deadline, &cancel)?
            }
            Err(error) => return Err(error.into()),
        }
    };
    setup_stage(8);
    let mode = PIPE_NOWAIT;
    if unsafe {
        SetNamedPipeHandleState(
            file.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(os_error());
    }
    setup_stage(9);
    let mut peer = 0;
    if unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut peer) } == 0
        || peer != parent_pid
    {
        return Err(Error::Request("read broker server PID mismatch".into()));
    }
    setup_stage(10);
    let mut pipe = Pipe { file, deadline };
    let request: Request = pipe.receive(&cancel)?;
    if request.nonce != nonce {
        return Err(Error::Request("read broker request mismatch".into()));
    }
    let operation = (|| -> Result<(File, crate::device::DeviceInfo)> {
        let source = source_path(&request.source)?;
        let (file, info) = crate::device::open(&source)?;
        Ok((
            file,
            info.ok_or_else(|| Error::Request("broker source is not a device".into()))?,
        ))
    })();
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
                },
                &cancel,
            )?;
            return Ok(());
        }
    };
    let remote = duplicate(
        unsafe { GetCurrentProcess() },
        file.as_raw_handle(),
        parent.0,
        false,
    )?;
    let _bootstrap = Bootstrap {
        process: parent.0,
        handle: remote,
    };
    pipe.send(
        &Response {
            nonce: nonce.into(),
            handle: remote as usize as u64,
            bytes: info.bytes,
            sector: info.sector,
            identity: info.identity,
            error: None,
        },
        &cancel,
    )?;
    let ack: Ack = pipe.receive(&cancel)?;
    if ack.nonce != nonce {
        return Err(Error::Request(
            "read broker acknowledgement mismatch".into(),
        ));
    }
    Ok(())
}
pub(super) fn entry() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_none_or(|s| s != FLAG) {
        return None;
    }
    let result = (|| -> Result<()> {
        if args.len() != 4 {
            return Err(Error::Request("read broker arguments".into()));
        }
        let args = args
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .ok_or_else(|| Error::Request("read broker argument encoding".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        let pid = args[2]
            .parse()
            .map_err(|_| Error::Request("read broker parent PID".into()))?;
        let created = args[3]
            .parse()
            .map_err(|_| Error::Request("read broker parent time".into()))?;
        helper(args[1], pid, created)
    })();
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            let os = match error {
                Error::Io(error) => error.raw_os_error().unwrap_or(0) as u32,
                _ => 0,
            };
            (0xa000_0000 | HELPER_STAGE.with(|s| s.get() << 24) | (os & 0x00ff_ffff)) as i32
        }
    })
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Transfer {
    source: PathBuf,
    handle: u64,
}
pub(crate) fn transfer_to(session: &Session, child: &std::process::Child) -> Result<Vec<Transfer>> {
    let paths = if let Some(raid) = &session.raid {
        if raid.config.members.len() > 16 {
            return Err(Error::Limit("preview device grants".into()));
        }
        raid.config
            .members
            .iter()
            .filter_map(|member| member.path.as_ref())
            .collect::<Vec<_>>()
    } else {
        vec![&session.source]
    };
    let mut transfers = Vec::new();
    for path in paths {
        let source = crate::device::canonical_source_path(path)?;
        crate::source_scope::check(&source)?;
        if let Some(file) = granted(&source)? {
            let handle = duplicate(
                unsafe { GetCurrentProcess() },
                file.as_raw_handle(),
                child.as_raw_handle(),
                false,
            )?;
            transfers.push(Transfer {
                source,
                handle: handle as usize as u64,
            });
        }
    }
    // If a later transfer fails, Child's guard terminates the child and releases
    // every earlier duplicate. The parent never owns these remote handles.
    Ok(transfers)
}
pub(crate) fn install_transferred(handles: Vec<Transfer>) -> Result<()> {
    if handles.len() > 16 {
        return Err(Error::Limit("preview device grants".into()));
    }
    for grant in handles {
        let source = source_path(&grant.source)?;
        let raw = usize::try_from(grant.handle)
            .map_err(|_| Error::Request("preview handle width".into()))?
            as HANDLE;
        let file = local_file(raw)?;
        crate::device::device_info(&file, &source)?;
        remember(source, file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonblocking_pipe_waits_for_a_delayed_frame_and_reports_disconnect() {
        let nonce = Uuid::new_v4().to_string();
        let mut inbound = server(&nonce).unwrap();
        inbound.deadline = Instant::now() + Duration::from_secs(2);
        unsafe {
            ConnectNamedPipe(inbound.file.as_raw_handle(), std::ptr::null_mut());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name(&nonce))
            .unwrap();
        let mode = PIPE_NOWAIT;
        assert_ne!(
            unsafe {
                SetNamedPipeHandleState(
                    file.as_raw_handle(),
                    &mode,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        let ok = unsafe { ConnectNamedPipe(inbound.file.as_raw_handle(), std::ptr::null_mut()) };
        assert!(
            ok != 0
                || std::io::Error::last_os_error().raw_os_error()
                    == Some(ERROR_PIPE_CONNECTED as i32)
        );
        let worker = std::thread::spawn(move || {
            let message: serde_json::Value = inbound.receive(&AtomicBool::new(false)).unwrap();
            assert_eq!(message, serde_json::json!({"frame":"delayed"}));
            inbound
        });
        std::thread::sleep(Duration::from_millis(75));
        let mut outbound = Pipe {
            file,
            deadline: Instant::now() + Duration::from_secs(2),
        };
        outbound
            .send(
                &serde_json::json!({"frame":"delayed"}),
                &AtomicBool::new(false),
            )
            .unwrap();
        let mut inbound = worker.join().unwrap();
        drop(outbound);
        let start = Instant::now();
        let error = inbound
            .receive::<serde_json::Value>(&AtomicBool::new(false))
            .unwrap_err();
        assert!(
            matches!(error,Error::Io(error) if error.raw_os_error()==Some(ERROR_BROKEN_PIPE as i32))
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn read_only_handle_rejects_write_delete_and_acl_access() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owned.bin");
        std::fs::write(&path, b"owned read access fixture").unwrap();
        let file = File::open(&path).unwrap();
        assert_ne!(
            read_rights(file.as_raw_handle()).unwrap() & FILE_READ_DATA,
            0
        );
        let writable = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(read_rights(writable.as_raw_handle()).is_err());
        for access in [
            GENERIC_READ | DELETE,
            GENERIC_READ | WRITE_DAC,
            GENERIC_READ | WRITE_OWNER,
        ] {
            let file = OpenOptions::new().access_mode(access).open(&path).unwrap();
            assert!(read_rights(file.as_raw_handle()).is_err());
        }
    }

    #[test]
    fn elevation_source_rejects_files_pipes_network_and_device_subpaths() {
        for path in [
            r"C:\owned.raw",
            r"\\server\share\owned.raw",
            r"\\.\pipe\owned",
            r"\\.\PhysicalDrive1\file",
            r"\\.\C:\file",
            r"\\?\GLOBALROOT\Device\Harddisk0\Partition0",
        ] {
            assert!(source_path(Path::new(path)).is_err(), "{path}");
        }
        let cancel = AtomicBool::new(true);
        assert!(matches!(
            request(Path::new(r"\\.\PhysicalDrive1"), &cancel),
            Err(Error::Cancelled)
        ));
    }
}
