//! Durable, resumable acquisition. Every map commit follows data sync.
use crate::{
    AcquisitionStrategy, Error, OutputRoot, RecoveryDestination, Result,
    read_map::{Header, IMAGE_NAME, MAP_NAME, ReadMap, ReadMapSummary, ReadState},
    source::Image,
};
use cap_std::fs::OpenOptions;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AcquisitionOptions {
    /// Must be a multiple of the source sector size, at most 16 MiB.
    pub block_size: u32,
    /// Additional passes after the first large-block pass (0..=3).
    pub retries: u8,
    pub resume: bool,
    /// None selects adaptive for new acquisitions, or the saved strategy on resume.
    pub strategy: Option<AcquisitionStrategy>,
    /// Per-request helper wait limit (100..=300000 ms); zero opts into direct OS reads.
    pub read_timeout_ms: u32,
    /// Maximum time to wait for the identical source to reconnect (0..=300000 ms).
    pub reconnect_wait_ms: u32,
}
impl Default for AcquisitionOptions {
    fn default() -> Self {
        Self {
            block_size: 4 * 1024 * 1024,
            retries: 2,
            resume: false,
            strategy: None,
            read_timeout_ms: 30_000,
            reconnect_wait_ms: 15_000,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionProgress {
    pub phase: String,
    pub pass: u8,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub map: ReadMapSummary,
    pub direction: String,
    pub block_size: u32,
    pub detail: Option<String>,
}
/// Statistics cover this invocation only, including a resumed invocation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionPass {
    pub pass: u8,
    pub direction: String,
    pub block_size: u32,
    pub attempts: u64,
    pub copied_bytes: u64,
    pub failed_bytes: u64,
    pub timeouts: u64,
    pub reconnects: u64,
    pub elapsed_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionResult {
    pub schema_version: u32,
    pub state: String,
    pub image: PathBuf,
    pub read_map: PathBuf,
    pub summary: ReadMapSummary,
    pub warnings: Vec<String>,
    pub passes: Vec<AcquisitionPass>,
    pub stop_reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadMapRange {
    pub offset: u64,
    pub length: u64,
    pub state: ReadState,
    pub attempts: u8,
    pub sha256: Option<String>,
    pub error_code: Option<i32>,
    pub read_unix_ms: Option<u64>,
    pub read_utc: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadMapPage {
    #[serde(flatten)]
    pub summary: ReadMapSummary,
    pub map_digest: String,
    pub ranges: Vec<ReadMapRange>,
    pub next_offset: Option<u64>,
}

fn lock_error(e: impl std::fmt::Display) -> Error {
    Error::Request(format!(
        "acquisition is in use by another reader or writer: {e}"
    ))
}
fn now_ms() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Invalid("system clock precedes Unix epoch".into()))?
        .as_millis()
        .try_into()
        .map_err(|_| Error::Invalid("timestamp overflow".into()))
}
fn regular_single_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::Request(
            "acquisition output must be a regular file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(Error::Request(
                "acquisition output cannot have hard links".into(),
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // Only query metadata on the already opened handle; never reopen an unchecked path.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if info.nNumberOfLinks != 1 {
            return Err(Error::Request(
                "acquisition output cannot have hard links".into(),
            ));
        }
    }
    Ok(())
}
fn verify_named(destination: &RecoveryDestination, name: &str, file: &File) -> Result<()> {
    destination.verify()?;
    let current = destination.dir.open(name)?.into_std();
    if same_file::Handle::from_file(current)? != same_file::Handle::from_file(file.try_clone()?)? {
        return Err(Error::DestinationChanged);
    }
    regular_single_file(file)
}

pub fn inspect_read_map(image: &Path) -> Result<ReadMapSummary> {
    let path = crate::source::map_path(image);
    let mut file = File::open(path)?;
    file.try_lock_shared().map_err(lock_error)?;
    Ok(ReadMap::load(&mut file)?.summary())
}
/// Paginate immutable intervals by source offset; the digest rejects a changed map between pages.
pub fn read_map_page(
    image: &Path,
    offset: u64,
    limit: usize,
    expected_digest: Option<&str>,
) -> Result<ReadMapPage> {
    if !(1..=1000).contains(&limit) {
        return Err(Error::Request("read-map page limit must be 1..1000".into()));
    }
    let mut file = File::open(crate::source::map_path(image))?;
    file.try_lock_shared().map_err(lock_error)?;
    let map = ReadMap::load(&mut file)?;
    let digest = map.digest();
    if expected_digest.is_some_and(|expected| expected != digest) {
        return Err(Error::SourceChanged);
    }
    if offset > map.header.source.length {
        return Err(Error::Request("read-map offset exceeds source".into()));
    }
    let mut iter = map.segments.range(offset..);
    let ranges = iter
        .by_ref()
        .take(limit)
        .map(|(_, s)| ReadMapRange {
            offset: s.offset,
            length: s.length,
            state: s.state,
            attempts: s.attempts,
            sha256: (s.state == ReadState::Copied)
                .then(|| s.sha256.iter().map(|b| format!("{b:02x}")).collect()),
            error_code: (s.state == ReadState::Failed).then_some(s.error_code),
            read_unix_ms: (s.read_unix_ms != 0).then_some(s.read_unix_ms),
            read_utc: s
                .read_unix_ms
                .checked_mul(10000)
                .and_then(|n| n.checked_add(116444736000000000))
                .and_then(crate::timestamps::filetime)
                .map(|t| t.value)
                .filter(|_| s.read_unix_ms != 0),
        })
        .collect();
    let next_offset = iter.next().map(|(at, _)| *at);
    Ok(ReadMapPage {
        summary: map.summary(),
        map_digest: digest,
        ranges,
        next_offset,
    })
}

/// Executables using timed reads must dispatch `acquisition_worker_entry` before argument parsing.
/// Library embedders can opt into direct reads with `read_timeout_ms = 0`.
pub fn acquire_image(
    source: &Path,
    destination: &Path,
    options: AcquisitionOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(AcquisitionProgress),
) -> Result<AcquisitionResult> {
    acquire_image_with_roots(source, destination, options, None, cancel, progress)
}
pub fn acquire_image_with_roots(
    source: &Path,
    destination: &Path,
    options: AcquisitionOptions,
    roots: Option<&[OutputRoot]>,
    cancel: &AtomicBool,
    progress: impl FnMut(AcquisitionProgress),
) -> Result<AcquisitionResult> {
    let mut source = Image::open(source)?;
    if source.has_read_map() {
        return Err(Error::Request(
            "an acquisition already has a read map; scan it or resume from its original source"
                .into(),
        ));
    }
    if options.retries > 3
        || (options.read_timeout_ms != 0 && !(100..=300_000).contains(&options.read_timeout_ms))
        || options.reconnect_wait_ms > 300_000
        || options.block_size > 16 * 1024 * 1024
        || options.block_size < source.sector_size()
        || !options.block_size.is_multiple_of(source.sector_size())
    {
        return Err(Error::Request(
            "block size must be sector aligned and <=16 MiB; retries must be 0..3; read timeout must be 0 or 100..300000 ms; reconnect wait must be 0..300000 ms".into(),
        ));
    }
    crate::guard_output_path(&source.path, destination)?;
    let destination = if let Some(roots) = roots {
        RecoveryDestination::within_create(destination, roots)?
    } else {
        RecoveryDestination::open(destination)?
    };
    crate::guard_output_path(&source.path, &destination.path)?;
    acquire_to(&mut source, &destination, options, cancel, progress)
}

fn acquire_to(
    source: &mut Image,
    destination: &RecoveryDestination,
    options: AcquisitionOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(AcquisitionProgress),
) -> Result<AcquisitionResult> {
    let identity = source.acquisition_identity()?;
    if !options.resume && destination.dir.entries()?.next().transpose()?.is_some() {
        return Err(Error::Request(
            "new acquisition requires an empty destination directory".into(),
        ));
    }
    let mut map_file = destination
        .dir
        .open_with(
            MAP_NAME,
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(!options.resume),
        )?
        .into_std();
    map_file.try_lock().map_err(lock_error)?;
    regular_single_file(&map_file)?;
    let mut map = if options.resume {
        let map = ReadMap::load(&mut map_file)?;
        if map.header.source != identity {
            return Err(Error::SourceChanged);
        }
        if map.header.block_size != options.block_size {
            return Err(Error::Request(
                "resume block size must match the existing read map".into(),
            ));
        }
        if options
            .strategy
            .is_some_and(|strategy| strategy != map.header.strategy)
        {
            return Err(Error::Request(
                "resume strategy must match the existing read map; use auto to preserve it".into(),
            ));
        }
        map
    } else {
        ReadMap::create(
            &mut map_file,
            Header {
                schema_version: if options.strategy == Some(AcquisitionStrategy::Linear) {
                    1
                } else {
                    2
                },
                strategy: options.strategy.unwrap_or(AcquisitionStrategy::Adaptive),
                id: uuid::Uuid::new_v4().to_string(),
                source: identity.clone(),
                block_size: options.block_size,
                image_name: IMAGE_NAME.into(),
                created_unix_ms: now_ms()?,
            },
        )?
    };
    let mut image = destination
        .dir
        .open_with(
            IMAGE_NAME,
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(!options.resume),
        )?
        .into_std();
    regular_single_file(&image)?;
    source.reject_same_file(&image)?;
    source.reject_same_file(&map_file)?;
    if same_file::Handle::from_file(image.try_clone()?)?
        == same_file::Handle::from_file(map_file.try_clone()?)?
    {
        return Err(Error::Request(
            "image and read map alias the same file".into(),
        ));
    }
    if image.metadata()?.len() > identity.length {
        return Err(Error::Invalid(
            "acquired image exceeds source length".into(),
        ));
    }
    #[cfg(unix)]
    // cap-std's directory capability may use O_PATH, which cannot be fsynced.
    destination.dir.open(".")?.sync_all()?;
    // Validate all previous successful blocks against the acquired image, not the damaged disk.
    let mut verified = 0;
    let copied = map.summary().copied_bytes;
    for segment in map
        .segments
        .values()
        .filter(|s| s.state == ReadState::Copied)
    {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut bytes = vec![0; segment.length as usize];
        image.seek(SeekFrom::Start(segment.offset))?;
        image.read_exact(&mut bytes)?;
        if Sha256::digest(&bytes).as_slice() != segment.sha256 {
            return Err(Error::Invalid(format!(
                "acquired data checksum mismatch at {}",
                segment.offset
            )));
        }
        verified += segment.length;
        progress(AcquisitionProgress {
            phase: "verify_resume".into(),
            pass: 0,
            completed_bytes: verified,
            total_bytes: copied,
            map: map.summary(),
            direction: "forward".into(),
            block_size: options.block_size,
            detail: None,
        });
    }
    let pinned_image = image.try_clone()?;
    let pinned_map = map_file.try_clone()?;
    let mut reader = crate::acquisition_reader::Reader::new(source, identity.clone(), &options);
    let mut outcome = run_passes(
        &mut map,
        &mut map_file,
        &mut image,
        options.retries,
        cancel,
        &mut progress,
        &mut reader,
        || {
            verify_named(destination, IMAGE_NAME, &pinned_image)?;
            verify_named(destination, MAP_NAME, &pinned_map)
        },
    )?;
    if !cancel.load(Ordering::Relaxed) && outcome.stop_reason.is_none() {
        match reader.verify(cancel) {
            Ok(()) => {}
            Err(Error::Cancelled) => {}
            Err(Error::Io(error)) => {
                outcome.stop_reason = Some(format!("final source verification failed: {error}"))
            }
            Err(error) => return Err(error),
        }
    }
    verify_named(destination, IMAGE_NAME, &image)?;
    verify_named(destination, MAP_NAME, &map_file)?;
    let summary = map.summary();
    let state = if cancel.load(Ordering::Relaxed) {
        "cancelled"
    } else if outcome.stop_reason.is_none()
        && summary.failed_bytes == 0
        && summary.unattempted_bytes == 0
    {
        "completed"
    } else {
        "partial"
    };
    Ok(AcquisitionResult{schema_version:2,state:state.into(),image:destination.path.join(IMAGE_NAME),read_map:destination.path.join(MAP_NAME),summary,passes:outcome.passes,stop_reason:outcome.stop_reason,warnings:vec!["Keep the RAW image together with its .rmap. Missing ranges are unavailable, never verified zero bytes. Mounted source media can change externally; native identity/geometry do not establish a point-in-time snapshot.".into(), "Read deadlines bound the application's wait for its helper, not kernel I/O cancellation or initial device discovery. Pass statistics describe this invocation only.".into()]})
}

#[derive(Default)]
struct RunOutcome {
    passes: Vec<AcquisitionPass>,
    stop_reason: Option<String>,
}

fn pass_plan(map: &ReadMap, pass: u8) -> (u32, bool) {
    let sector = map.header.source.sector_size;
    match (map.header.strategy, pass) {
        (_, 0) => (map.header.block_size, false),
        (AcquisitionStrategy::Adaptive, 1) => (
            ((map.header.block_size / 16).min(65536) / sector * sector).max(sector),
            true,
        ),
        (AcquisitionStrategy::Adaptive, 3) => (sector, true),
        _ => (sector, false),
    }
}

/// Separated I/O allows deterministic bad-sector, interruption and failed-write tests.
#[allow(clippy::too_many_arguments)]
fn run_passes(
    map: &mut ReadMap,
    map_file: &mut File,
    image: &mut File,
    retries: u8,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(AcquisitionProgress),
    read: &mut impl crate::acquisition_reader::ReadSource,
    mut verify: impl FnMut() -> Result<()>,
) -> Result<RunOutcome> {
    let mut outcome = RunOutcome::default();
    for pass in 0..=retries {
        let started = Instant::now();
        let (block_size, reverse) = pass_plan(map, pass);
        let direction = if reverse { "reverse" } else { "forward" };
        let mut stats = AcquisitionPass {
            pass: pass + 1,
            direction: direction.into(),
            block_size,
            attempts: 0,
            copied_bytes: 0,
            failed_bytes: 0,
            timeouts: 0,
            reconnects: 0,
            elapsed_ms: 0,
        };
        let mut at = if reverse { map.header.source.length } else { 0 };
        while let Some(mut segment) = map.next_attempt_in_direction(at, pass, block_size, reverse) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            verify()?;
            let mut failure = None;
            match read.read(segment.offset, segment.length as usize, cancel) {
                Ok(bytes) => {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if bytes.len() != segment.length as usize {
                        return Err(Error::Invalid("short source read".into()));
                    }
                    segment.state = ReadState::Copied;
                    segment.sha256 = Sha256::digest(&bytes).into();
                    image.seek(SeekFrom::Start(segment.offset))?;
                    image.write_all(&bytes)?;
                    image.sync_data()?;
                    stats.copied_bytes += segment.length;
                }
                Err(error @ Error::MissingRanges(_)) => {
                    segment.error_code = -2;
                    failure = Some(error);
                }
                Err(Error::Io(error)) => {
                    segment.error_code = error.raw_os_error().unwrap_or(-1);
                    if error.kind() == std::io::ErrorKind::TimedOut {
                        segment.error_code = -3;
                        stats.timeouts += 1;
                    }
                    failure = Some(Error::Io(error));
                }
                Err(Error::Cancelled) => break,
                Err(error) => return Err(error),
            }
            // A full write and data sync must succeed before successful bytes enter the journal.
            segment.read_unix_ms = now_ms()?;
            verify()?;
            map.append(map_file, segment.clone())?;
            stats.attempts += 1;
            if failure.is_some() {
                stats.failed_bytes += segment.length;
            }
            at = if reverse {
                segment.offset
            } else {
                segment.end()
            };
            let summary = map.summary();
            progress(AcquisitionProgress {
                phase: if pass == 0 { "acquire" } else { "retry" }.into(),
                pass: pass + 1,
                completed_bytes: summary.copied_bytes,
                total_bytes: summary.length,
                map: summary,
                direction: direction.into(),
                block_size,
                detail: None,
            });
            if let Some(error) = failure {
                let recovery = read.recover(&error, cancel, &mut |detail| {
                    progress(AcquisitionProgress {
                        phase: "reconnect".into(),
                        pass: pass + 1,
                        completed_bytes: map.summary().copied_bytes,
                        total_bytes: map.header.source.length,
                        map: map.summary(),
                        direction: direction.into(),
                        block_size,
                        detail: Some(detail.into()),
                    })
                });
                match recovery {
                    Ok(crate::acquisition_reader::Recovery::Continue { reconnects }) => {
                        stats.reconnects += reconnects
                    }
                    Ok(crate::acquisition_reader::Recovery::Stop(reason)) => {
                        outcome.stop_reason = Some(reason);
                        break;
                    }
                    Err(Error::Cancelled) => break,
                    Err(error) => return Err(error),
                }
            }
        }
        stats.elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        if stats.attempts != 0 {
            outcome.passes.push(stats);
        }
        if cancel.load(Ordering::Relaxed) || outcome.stop_reason.is_some() {
            break;
        }
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AcquisitionIdentity, read_map::Segment};
    fn fixture(length: u64, block_size: u32) -> (tempfile::TempDir, File, File, ReadMap) {
        fixture_strategy(length, block_size, AcquisitionStrategy::Linear)
    }
    fn fixture_strategy(
        length: u64,
        block_size: u32,
        strategy: AcquisitionStrategy,
    ) -> (tempfile::TempDir, File, File, ReadMap) {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.path().join(MAP_NAME))
            .unwrap();
        let image = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.path().join(IMAGE_NAME))
            .unwrap();
        let header = Header {
            schema_version: if strategy == AcquisitionStrategy::Linear {
                1
            } else {
                2
            },
            strategy,
            id: uuid::Uuid::new_v4().to_string(),
            source: AcquisitionIdentity {
                path: "synthetic".into(),
                length,
                sector_size: 512,
                identity: "fault injection".into(),
                modified_ns: None,
            },
            block_size,
            image_name: IMAGE_NAME.into(),
            created_unix_ms: 1,
        };
        let map = ReadMap::create(&mut journal, header).unwrap();
        (dir, journal, image, map)
    }
    #[test]
    fn adaptive_passes_reverse_subdivide_and_preserve_successful_ranges() {
        let (_dir, mut journal, mut image, mut map) =
            fixture_strategy(131073, 65536, AcquisitionStrategy::Adaptive);
        let data = (0..131073).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        let mut calls = Vec::new();
        let mut tail_failed = false;
        let outcome = run_passes(
            &mut map,
            &mut journal,
            &mut image,
            3,
            &AtomicBool::new(false),
            &mut |_| {},
            &mut |at, len| {
                calls.push((at, len));
                if len > 512
                    || at == 512
                    || (at == 131072 && !std::mem::replace(&mut tail_failed, true))
                {
                    return Err(std::io::Error::other("injected unreadable range").into());
                }
                Ok(data[at as usize..at as usize + len].to_vec())
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            outcome
                .passes
                .iter()
                .map(|p| (p.direction.as_str(), p.block_size))
                .collect::<Vec<_>>(),
            vec![
                ("forward", 65536),
                ("reverse", 4096),
                ("forward", 512),
                ("reverse", 512)
            ]
        );
        let reverse = &calls[3..36];
        assert!(reverse.windows(2).all(|p| p[0].0 > p[1].0));
        assert_eq!(reverse[0], (131072, 1));
        assert_eq!(reverse[1], (126976, 4096));
        // Every request is bounded and starts on a sector boundary, including the one-byte tail.
        assert!(calls.iter().all(|(at, _)| at % 512 == 0));
        assert_eq!(map.summary().copied_bytes, 131073 - 512);
        assert_eq!(map.summary().failed_bytes, 512);
        assert_eq!(map.summary().unattempted_bytes, 0);
        assert_eq!(map.segments[&512].attempts, 4);
        let saved = ReadMap::load(&mut journal).unwrap();
        assert_eq!(saved.header.strategy, AcquisitionStrategy::Adaptive);
        let mut rereads = 0;
        run_passes(
            &mut map,
            &mut journal,
            &mut image,
            3,
            &AtomicBool::new(false),
            &mut |_| {},
            &mut |_, _| {
                rereads += 1;
                unreachable!()
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(rereads, 0);
        image.seek(SeekFrom::Start(1024)).unwrap();
        let mut actual = Vec::new();
        image.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, data[1024..]);
    }
    #[test]
    fn retries_preserve_permanent_gaps_and_recover_transient_sectors() {
        let (dir, mut journal, mut image, mut map) = fixture(8192, 2048);
        let data = (0..8192).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        let mut calls = std::collections::BTreeMap::new();
        run_passes(
            &mut map,
            &mut journal,
            &mut image,
            2,
            &AtomicBool::new(false),
            &mut |_| {},
            &mut |at, len| {
                let count = calls.entry(at).or_insert(0);
                *count += 1;
                if (at <= 512 && at + len as u64 > 512) || (at == 1024 && *count == 1) {
                    return Err(std::io::Error::other("synthetic read fault").into());
                }
                Ok(data[at as usize..at as usize + len].to_vec())
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(map.summary().copied_bytes, 7680);
        assert_eq!(map.summary().failed_bytes, 512);
        assert_eq!(map.summary().unattempted_bytes, 0);
        assert_eq!(map.segments[&512].attempts, 3);
        assert_eq!(map.segments[&1024].attempts, 3);
        drop(image);
        drop(journal);
        let mut source = Image::open(&dir.path().join(IMAGE_NAME)).unwrap();
        assert_eq!(source.read(1024, 7168).unwrap(), data[1024..]);
        let Error::MissingRanges(gaps) = source.read(500, 30).unwrap_err() else {
            panic!("gap read became data")
        };
        assert_eq!(gaps[0].source_offset, 512);
        assert_eq!(gaps[0].length, 18);
        // Mapping to a very large logical stream must subtract physical offsets
        // before adding the logical offset, avoiding an intermediate overflow.
        let logical = u64::MAX - 511;
        let stream = crate::ntfs::Data::NonResident {
            runs: vec![crate::ntfs::Run {
                logical,
                physical: Some(512),
                length: 511,
            }],
            size: u64::MAX,
            initialized: u64::MAX,
        };
        let Error::MissingRanges(gaps) = stream.read(&mut source, logical, 100).unwrap_err() else {
            panic!("gap lost")
        };
        assert_eq!(gaps[0].logical_offset, Some(logical));
        let (available, gaps) = source.read_available(0, 8192).unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(&available[1024..], &data[1024..]);
        // The source fingerprint includes the map, and succeeds even if a sample crosses a gap.
        assert!(
            source
                .fingerprint()
                .unwrap()
                .modified_ns
                .starts_with("mapped:")
        );
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join(MAP_NAME))
            .unwrap();
        assert!(
            held.try_lock().is_err(),
            "an active scan must prevent acquisition writes"
        );
    }
    #[test]
    fn crash_tail_is_ignored_and_resume_never_rereads_copied_blocks() {
        let (_dir, mut journal, mut image, mut map) = fixture(4097, 1024);
        let cancel = AtomicBool::new(false);
        run_passes(
            &mut map,
            &mut journal,
            &mut image,
            0,
            &cancel,
            &mut |p| {
                if p.map.copied_bytes >= 1024 {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            &mut |at, len| Ok((at..at + len as u64).map(|v| (v % 251) as u8).collect()),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(map.summary().copied_bytes, 1024);
        journal
            .write_all(b"partial record written at crash")
            .unwrap();
        journal.sync_all().unwrap();
        let mut map = ReadMap::load(&mut journal).unwrap();
        assert!(map.ignored_tail);
        cancel.store(false, Ordering::Relaxed);
        run_passes(
            &mut map,
            &mut journal,
            &mut image,
            0,
            &cancel,
            &mut |_| {},
            &mut |at, len| {
                assert!(at >= 1024);
                Ok((at..at + len as u64).map(|v| (v % 251) as u8).collect())
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(map.summary().copied_bytes, 4097);
        assert!(!map.ignored_tail);
        assert_eq!(
            ReadMap::load(&mut journal).unwrap().summary().copied_bytes,
            4097
        );
        let mut bytes = Vec::new();
        image.seek(SeekFrom::Start(0)).unwrap();
        image.read_to_end(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            (0..4097).map(|v| (v % 251) as u8).collect::<Vec<_>>()
        );
    }
    #[test]
    fn failed_output_write_cannot_become_a_copied_record() {
        let (dir, mut journal, image, mut map) = fixture(4096, 1024);
        drop(image);
        let mut readonly = File::open(dir.path().join(IMAGE_NAME)).unwrap();
        assert!(
            run_passes(
                &mut map,
                &mut journal,
                &mut readonly,
                0,
                &AtomicBool::new(false),
                &mut |_| {},
                &mut |_, len| Ok(vec![3; len]),
                || Ok(())
            )
            .is_err()
        );
        assert_eq!(
            ReadMap::load(&mut journal).unwrap().summary().copied_bytes,
            0
        );
        assert_eq!(map.summary().unattempted_bytes, 4096);
    }
    #[test]
    fn map_crc_sequence_bounds_and_immutable_blocks_fail_closed() {
        let (_dir, mut journal, _image, mut map) = fixture(4096, 1024);
        let segment = Segment {
            offset: 0,
            length: 1024,
            state: ReadState::Copied,
            attempts: 1,
            sha256: [1; 32],
            error_code: 0,
            read_unix_ms: 1,
        };
        map.append(&mut journal, segment.clone()).unwrap();
        let first = map.durable_length;
        assert!(map.append(&mut journal, segment.clone()).is_err());
        assert_eq!(journal.metadata().unwrap().len(), first);
        let mut invalid = segment.clone();
        invalid.offset = u64::MAX - 1023;
        assert!(map.append(&mut journal, invalid).is_err());
        let mut next = segment;
        next.offset = 1024;
        map.append(&mut journal, next).unwrap();
        // Corrupt an earlier record, never silently discard an interior corruption.
        journal.seek(SeekFrom::Start(first - 1)).unwrap();
        let mut checksum = [0];
        journal.read_exact(&mut checksum).unwrap();
        checksum[0] ^= 1;
        journal.seek(SeekFrom::Start(first - 1)).unwrap();
        journal.write_all(&checksum).unwrap();
        assert!(ReadMap::load(&mut journal).is_err());
    }
    #[test]
    fn checksum_or_missing_sidecar_prevents_scanning_a_reserved_image() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.raw");
        std::fs::write(&source, vec![7; 8192]).unwrap();
        let destination = root.path().join("acquired");
        let result = acquire_image(
            &source,
            &destination,
            AcquisitionOptions {
                read_timeout_ms: 0,
                ..Default::default()
            },
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(result.state, "completed");
        let mut bytes = std::fs::read(&result.image).unwrap();
        bytes[123] ^= 1;
        std::fs::write(&result.image, bytes).unwrap();
        assert!(Image::open(&result.image).unwrap().read(0, 512).is_err());
        assert!(
            acquire_image(
                &source,
                &destination,
                AcquisitionOptions {
                    resume: true,
                    ..Default::default()
                },
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        std::fs::remove_file(&result.read_map).unwrap();
        assert!(Image::open(&result.image).is_err());
    }
}
