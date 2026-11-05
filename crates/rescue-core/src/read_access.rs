//! Explicit Windows read permission; the GUI and parsers remain unelevated.
use crate::*;
#[cfg(windows)]
mod windows;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadAccess {
    pub source: PathBuf,
    pub bytes: u64,
    pub sector_size: u32,
    pub read_only: bool,
    pub granted_access: u32,
    pub parent_elevated: bool,
    pub helper_exited: bool,
}
pub fn request_read_access(source: &Path, cancel: &AtomicBool) -> Result<ReadAccess> {
    #[cfg(windows)]
    {
        windows::request(source, cancel)
    }
    #[cfg(not(windows))]
    {
        let _ = (source, cancel);
        Err(Error::Unsupported(
            "Windows read-permission broker is not used on this OS".into(),
        ))
    }
}
pub fn read_broker_entry() -> Option<i32> {
    #[cfg(windows)]
    {
        windows::entry()
    }
    #[cfg(not(windows))]
    {
        None
    }
}
#[cfg(windows)]
pub(crate) use windows::{Transfer, granted, install_transferred, transfer_to};
