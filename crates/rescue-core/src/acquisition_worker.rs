//! Read-only acquisition transport. The helper never receives an output path.
//! Waiting is bounded; a kernel-stuck child is quarantined, not multiplied.
use crate::{AcquisitionIdentity, Error, Result, SourceScope, source::Image, source_scope};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const FLAG: &str = "--rescue-acquisition-worker";
const MAX_CONTROL: usize = 64 * 1024;
const MAX_DATA: usize = 16 * 1024 * 1024;
const REAP_TIME: Duration = Duration::from_millis(250);
static LIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);

struct Permit;
impl Permit {
    fn acquire() -> Result<Self> {
        LIVE_WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 4).then_some(n + 1))
            .map_err(|_| Error::Limit("four acquisition readers are still active; wait for outstanding OS reads to end".into()))?;
        Ok(Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        LIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Operation {
    Open {
        source: PathBuf,
        identity: AcquisitionIdentity,
        grants: Option<Vec<PathBuf>>,
        #[cfg(windows)]
        #[serde(default)]
        handles: Vec<crate::read_access::Transfer>,
    },
    Read {
        offset: u64,
        length: usize,
    },
    Verify,
}
#[derive(Serialize, Deserialize)]
struct Request {
    nonce: String,
    sequence: u64,
    #[serde(flatten)]
    operation: Operation,
}
#[derive(Serialize, Deserialize)]
struct Failure {
    code: String,
    message: String,
    os_code: Option<i32>,
    io_kind: Option<String>,
}
impl Failure {
    fn from_error(error: Error) -> Self {
        let (os_code, io_kind) = if let Error::Io(e) = &error {
            (e.raw_os_error(), Some(format!("{:?}", e.kind())))
        } else {
            (None, None)
        };
        Self {
            code: error.code().into(),
            message: error.to_string().chars().take(1024).collect(),
            os_code,
            io_kind,
        }
    }
    fn into_error(self) -> Error {
        match self.code.as_str() {
            "SOURCE_CHANGED" => Error::SourceChanged,
            "MISSING_RANGES" => Error::MissingRanges(Vec::new()),
            "IO_ERROR" => {
                let kind = match self.io_kind.as_deref() {
                    Some("TimedOut") => std::io::ErrorKind::TimedOut,
                    Some("NotFound") => std::io::ErrorKind::NotFound,
                    Some("PermissionDenied") => std::io::ErrorKind::PermissionDenied,
                    Some("UnexpectedEof") => std::io::ErrorKind::UnexpectedEof,
                    _ => std::io::ErrorKind::Other,
                };
                self.os_code
                    .map_or_else(
                        || std::io::Error::new(kind, self.message),
                        std::io::Error::from_raw_os_error,
                    )
                    .into()
            }
            "RESOURCE_LIMIT" => Error::Limit(self.message),
            "UNSUPPORTED" => Error::Unsupported(self.message),
            "INVALID_REQUEST" => Error::Request(self.message),
            _ => Error::Invalid(format!(
                "acquisition reader {}: {}",
                self.code, self.message
            )),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Reply {
    nonce: String,
    sequence: u64,
    data_length: usize,
    error: Option<Failure>,
}

fn read_control(input: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut size = [0u8; 4];
    if input.read(&mut size[..1])? == 0 {
        return Ok(None);
    }
    input.read_exact(&mut size[1..])?;
    let count = u32::from_le_bytes(size) as usize;
    if !(1..=MAX_CONTROL).contains(&count) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "acquisition control frame bounds",
        ));
    }
    let mut bytes = vec![0; count];
    input.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}
fn write_control(output: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    if !(1..=MAX_CONTROL).contains(&bytes.len()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "acquisition control frame bounds",
        ));
    }
    output.write_all(&(bytes.len() as u32).to_le_bytes())?;
    output.write_all(bytes)
}

type Response = std::io::Result<(Reply, Vec<u8>)>;
pub(crate) struct Worker {
    child: Option<Child>,
    permit: Option<Permit>,
    input: Option<SyncSender<Vec<u8>>>,
    responses: Receiver<Response>,
    nonce: String,
    sequence: u64,
    pub reaped: bool,
}
impl Worker {
    pub fn start(
        source: &Image,
        identity: &AcquisitionIdentity,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let permit = Permit::acquire()?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg(FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn()?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| Error::Invalid("acquisition stdout pipe".into()))?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Invalid("acquisition stdin pipe".into()))?;
        let (send, receive) = mpsc::sync_channel::<Vec<u8>>(1);
        thread::spawn(move || {
            while let Ok(bytes) = receive.recv() {
                if write_control(&mut input, &bytes)
                    .and_then(|()| input.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        let (send_response, responses) = mpsc::sync_channel(1);
        thread::spawn(move || {
            loop {
                let result = (|| {
                    let bytes = read_control(&mut output)?.ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "acquisition reader exited",
                        )
                    })?;
                    let reply: Reply =
                        serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                    if reply.data_length > MAX_DATA
                        || (reply.error.is_some() && reply.data_length != 0)
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "acquisition response bounds",
                        ));
                    }
                    let mut data = vec![0; reply.data_length];
                    output.read_exact(&mut data)?;
                    Ok((reply, data))
                })();
                let failed = result.is_err();
                if send_response.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let mut worker = Self {
            child: Some(child),
            permit: Some(permit),
            input: Some(send),
            responses,
            nonce: uuid::Uuid::new_v4().to_string(),
            sequence: 0,
            reaped: true,
        };
        #[cfg(windows)]
        let handles = {
            let paths = source.read_grant_paths();
            crate::read_access::transfer_sources(
                &paths.iter().collect::<Vec<_>>(),
                worker.child.as_ref().unwrap(),
            )?
        };
        let opened = worker.exchange(
            Operation::Open {
                source: source.path.clone(),
                identity: identity.clone(),
                grants: source_scope::snapshot(),
                #[cfg(windows)]
                handles,
            },
            timeout,
            cancel,
        );
        if let Err(error) = opened {
            if !worker.stop() {
                return Err(Error::Limit(
                    "acquisition reader remains blocked in the OS; no replacement was started"
                        .into(),
                ));
            }
            return Err(error);
        }
        Ok(worker)
    }
    fn exchange(
        &mut self,
        operation: Operation,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>> {
        if cancel.load(Ordering::Relaxed) {
            self.stop();
            return Err(Error::Cancelled);
        }
        let expected_length = match operation {
            Operation::Read { length, .. } => length,
            _ => 0,
        };
        let request = Request {
            nonce: self.nonce.clone(),
            sequence: self.sequence,
            operation,
        };
        let bytes = serde_json::to_vec(&request)?;
        if bytes.len() > MAX_CONTROL {
            return Err(Error::Limit(
                "acquisition reader request exceeds 64 KiB".into(),
            ));
        }
        self.input
            .as_ref()
            .ok_or_else(|| Error::Invalid("acquisition reader stopped".into()))?
            .try_send(bytes)
            .map_err(|_| Error::Invalid("acquisition reader request pipe unavailable".into()))?;
        let started = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                self.stop();
                return Err(Error::Cancelled);
            }
            let Some(left) = timeout.checked_sub(started.elapsed()) else {
                self.stop();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "acquisition source read exceeded its wait limit",
                )
                .into());
            };
            match self
                .responses
                .recv_timeout(left.min(Duration::from_millis(20)))
            {
                Ok(Ok((reply, data))) => {
                    // A late reply must never reach the image writer.
                    if cancel.load(Ordering::Relaxed) {
                        self.stop();
                        return Err(Error::Cancelled);
                    }
                    if started.elapsed() >= timeout {
                        self.stop();
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "acquisition reply arrived after its wait limit",
                        )
                        .into());
                    }
                    if reply.nonce != self.nonce || reply.sequence != self.sequence {
                        self.stop();
                        return Err(Error::Invalid(
                            "acquisition reader response identity".into(),
                        ));
                    }
                    self.sequence = self
                        .sequence
                        .checked_add(1)
                        .ok_or_else(|| Error::Limit("acquisition requests".into()))?;
                    if let Some(error) = reply.error {
                        return Err(error.into_error());
                    }
                    if data.len() != expected_length {
                        self.stop();
                        return Err(Error::Invalid("short acquisition reader result".into()));
                    }
                    return Ok(data);
                }
                Ok(Err(error)) => {
                    self.stop();
                    return Err(Error::Invalid(format!(
                        "acquisition reader transport: {error}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop();
                    return Err(Error::Invalid(
                        "acquisition reader response pipe closed".into(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    pub fn read(
        &mut self,
        offset: u64,
        length: usize,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>> {
        self.exchange(Operation::Read { offset, length }, timeout, cancel)
    }
    pub fn verify(&mut self, timeout: Duration, cancel: &AtomicBool) -> Result<()> {
        self.exchange(Operation::Verify, timeout, cancel)
            .map(|_| ())
    }
    /// Never block Drop forever on a driver stuck in an uninterruptible read.
    pub fn stop(&mut self) -> bool {
        self.input.take();
        let Some(mut child) = self.child.take() else {
            return self.reaped;
        };
        let _ = child.kill();
        let until = Instant::now() + REAP_TIME;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.permit.take();
                    self.reaped = true;
                    return true;
                }
                _ if Instant::now() >= until => break,
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
        self.reaped = false;
        let permit = self.permit.take();
        thread::spawn(move || {
            let _ = child.wait();
            drop(permit);
        });
        false
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Private executable mode; framing and all source reads are bounded in size.
/// A separate stdin watcher exits on parent death even if the read is blocked.
pub fn acquisition_worker_entry() -> Option<i32> {
    if std::env::args_os().skip(1).collect::<Vec<_>>() != [std::ffi::OsString::from(FLAG)] {
        return None;
    }
    let run = || -> Result<()> {
        let (sender, input) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            loop {
                match read_control(&mut stdin) {
                    Ok(Some(bytes)) => {
                        if sender.send(bytes).is_err() {
                            return;
                        }
                    }
                    Ok(None) => std::process::exit(0),
                    Err(_) => std::process::exit(4),
                }
            }
        });
        let mut image: Option<Image> = None;
        let mut identity: Option<AcquisitionIdentity> = None;
        let mut scope: Option<SourceScope> = None;
        let mut nonce = String::new();
        let mut sequence = 0u64;
        let mut output = std::io::stdout().lock();
        while let Ok(bytes) = input.recv() {
            let request: Request = serde_json::from_slice(&bytes)?;
            if request.sequence != sequence || (sequence != 0 && request.nonce != nonce) {
                return Err(Error::Request("acquisition worker sequence".into()));
            }
            if sequence == 0 {
                uuid::Uuid::parse_str(&request.nonce)
                    .map_err(|_| Error::Request("acquisition worker nonce".into()))?;
                nonce.clone_from(&request.nonce);
            }
            let result = (|| -> Result<Vec<u8>> {
                match request.operation {
                    Operation::Open {
                        source,
                        identity: expected,
                        grants,
                        #[cfg(windows)]
                        handles,
                    } if sequence == 0 => {
                        scope = grants.map(SourceScope::enter);
                        #[cfg(windows)]
                        crate::read_access::install_transferred(handles)?;
                        let opened = Image::open(&source)?;
                        if opened.has_read_map() || opened.acquisition_identity()? != expected {
                            return Err(Error::SourceChanged);
                        }
                        image = Some(opened);
                        identity = Some(expected);
                        Ok(Vec::new())
                    }
                    Operation::Read { offset, length } if (1..=MAX_DATA).contains(&length) => image
                        .as_mut()
                        .ok_or_else(|| Error::Request("acquisition reader not open".into()))?
                        .read(offset, length),
                    Operation::Verify => {
                        if image
                            .as_ref()
                            .ok_or_else(|| Error::Request("acquisition reader not open".into()))?
                            .acquisition_identity()?
                            != *identity.as_ref().unwrap()
                        {
                            return Err(Error::SourceChanged);
                        }
                        Ok(Vec::new())
                    }
                    _ => Err(Error::Request(
                        "acquisition reader operation or bounds".into(),
                    )),
                }
            })();
            let (data, error) = match result {
                Ok(bytes) => (bytes, None),
                Err(error) => (Vec::new(), Some(Failure::from_error(error))),
            };
            let reply = Reply {
                nonce: nonce.clone(),
                sequence,
                data_length: data.len(),
                error,
            };
            write_control(&mut output, &serde_json::to_vec(&reply)?)?;
            output.write_all(&data)?;
            output.flush()?;
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| Error::Limit("acquisition requests".into()))?;
        }
        drop(scope);
        Ok(())
    };
    Some(if run().is_ok() { 0 } else { 4 })
}
