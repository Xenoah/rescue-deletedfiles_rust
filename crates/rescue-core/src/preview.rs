//! Bounded previews in a disposable process. No viewer, shell, URLs or macros.
//! Resource isolation is not a claim of a complete OS security sandbox.
use crate::*;
use std::{
    cell::Cell,
    io::Cursor,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
mod limits;

const WORKER: &str = "--rescue-preview-worker";
const MAX_REQUEST: usize = 2 * 1024 * 1024;
const MAX_REPLY: usize = 5 * 1024 * 1024;
const MAX_ENCODED: u64 = 8 * 1024 * 1024;
const MAX_PIXELS: usize = 16 * 1024 * 1024;
const MAX_TEXT: usize = 64 * 1024;
const MAX_READ: u64 = 64 * 1024 * 1024;
const WALL_TIME: Duration = Duration::from_secs(5);
thread_local! {static READ_LEFT:Cell<Option<u64>>=const{Cell::new(None)};}

/// Charge actual source reads (including aligned reads and map verification),
/// across every image opened by this worker. Other operations have no budget.
pub(crate) fn charge_read(length: usize) -> Result<()> {
    READ_LEFT.with(|slot| {
        if let Some(left) = slot.get() {
            slot.set(Some(left.checked_sub(length as u64).ok_or_else(|| {
                Error::Limit("preview source reads exceed 64 MiB".into())
            })?));
        }
        Ok(())
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreviewInfo {
    pub schema_version: u32,
    pub candidate_id: String,
    pub format: String,
    pub source_bytes: u64,
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    pub truncated: bool,
    pub source_bytes_read: u64,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preview {
    pub info: PreviewInfo,
    pub rgba: Vec<u8>,
    pub text: String,
}
impl Preview {
    fn validate(&self) -> Result<()> {
        let i = &self.info;
        let valid = i.schema_version == 1
            && i.source_bytes_read <= MAX_READ
            && i.source_bytes <= MAX_ENCODED
            && i.source_width as u64 * i.source_height as u64 <= MAX_PIXELS as u64
            && i.width <= 512
            && i.height <= 512
            && match i.format.as_str() {
                "png" | "jpeg" => {
                    i.width > 0
                        && i.height > 0
                        && self.text.is_empty()
                        && self.rgba.len() == i.width as usize * i.height as usize * 4
                }
                "text" => {
                    i.width == 0
                        && i.height == 0
                        && self.rgba.is_empty()
                        && self.text.len() <= 128 * 1024
                }
                _ => false,
            };
        if valid {
            Ok(())
        } else {
            Err(Error::Invalid("preview response bounds".into()))
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Request {
    nonce: String,
    session: Session,
    grants: Option<Vec<PathBuf>>,
    #[cfg(windows)]
    #[serde(default)]
    handles: Vec<crate::read_access::Transfer>,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    nonce: String,
    preview: Option<Preview>,
    error: Option<(String, String)>,
}
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Only the requested row is sent to the worker. It reopens the source read-only,
/// revalidates metadata and decodes under process, pixel, input and read limits.
pub fn preview_candidate(mut session: Session, id: &str, cancel: &AtomicBool) -> Result<Preview> {
    session.candidates.retain(|c| c.id == id);
    if session.candidates.len() != 1 {
        return Err(Error::Request(
            "preview needs exactly one known candidate".into(),
        ));
    }
    source_scope::check(&canonical_source_path(&session.source)?)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let nonce = Uuid::new_v4().to_string();
    #[allow(unused_mut)]
    let mut request = Request {
        nonce: nonce.clone(),
        session,
        grants: source_scope::snapshot(),
        #[cfg(windows)]
        handles: Vec::new(),
    };
    if serialized_size(&request)? > MAX_REQUEST as u64 {
        return Err(Error::Limit("preview metadata exceeds 2 MiB".into()));
    }
    let started = Instant::now();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(WORKER)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW, including GUI worker.
    }
    let mut child = Child(command.spawn()?);
    #[cfg(windows)]
    {
        request.handles = crate::read_access::transfer_to(&request.session, &child.0)?;
    }
    let bytes = serde_json::to_vec(&request)?;
    if bytes.len() > MAX_REQUEST {
        return Err(Error::Limit("preview metadata exceeds 2 MiB".into()));
    }
    let mut input = child
        .0
        .stdin
        .take()
        .ok_or_else(|| Error::Invalid("preview pipe".into()))?;
    let output = child
        .0
        .stdout
        .take()
        .ok_or_else(|| Error::Invalid("preview pipe".into()))?;
    let writer = thread::spawn(move || input.write_all(&bytes));
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = output
            .take(MAX_REPLY as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = tx.send(result);
    });
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if started.elapsed() >= WALL_TIME {
            return Err(Error::Limit(
                "preview exceeded 5 second wall-time limit".into(),
            ));
        }
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        limits::check(child.0.id())?;
        thread::sleep(Duration::from_millis(10));
    };
    // Closing the worker also closes both pipes. Bounded threads cannot retain
    // unbounded input/output, including when a decoder crashes or is killed.
    let _ = writer.join();
    let _ = reader.join();
    if !status.success() {
        return Err(Error::Limit(
            "preview worker stopped (resource limit or decoder failure)".into(),
        ));
    }
    let bytes = rx
        .recv()
        .map_err(|_| Error::Invalid("preview reply missing".into()))??;
    if bytes.len() > MAX_REPLY {
        return Err(Error::Limit("preview reply exceeds 5 MiB".into()));
    }
    let reply: Reply = serde_json::from_slice(&bytes)?;
    if reply.nonce != nonce {
        return Err(Error::Invalid("preview request mismatch".into()));
    }
    if let Some((code, message)) = reply.error {
        return Err(match code.as_str() {
            "SOURCE_CHANGED" => Error::SourceChanged,
            "RESOURCE_LIMIT" => Error::Limit(message),
            "UNSUPPORTED" => Error::Unsupported(message),
            "CANCELLED" => Error::Cancelled,
            _ => Error::Invalid(format!("preview {code}: {message}")),
        });
    }
    let preview = reply
        .preview
        .ok_or_else(|| Error::Invalid("preview result missing".into()))?;
    preview.validate()?;
    if preview.info.candidate_id != id {
        return Err(Error::SourceChanged);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(preview)
}

/// Called before CLI parsing or GUI initialization. This private mode accepts
/// bounded stdin only and cannot publish output files or launch other programs.
pub fn preview_worker_entry() -> Option<i32> {
    if std::env::args_os().skip(1).collect::<Vec<_>>() != [std::ffi::OsString::from(WORKER)] {
        return None;
    }
    let run = || -> Result<()> {
        #[cfg(windows)]
        let _limits = limits::install()?;
        #[cfg(unix)]
        limits::install()?;
        let mut input = Vec::new();
        std::io::stdin()
            .take(MAX_REQUEST as u64 + 1)
            .read_to_end(&mut input)?;
        if input.len() > MAX_REQUEST {
            return Err(Error::Limit("preview request".into()));
        }
        let request: Request = serde_json::from_slice(&input)?;
        let _scope = request.grants.map(SourceScope::enter);
        #[cfg(windows)]
        crate::read_access::install_transferred(request.handles)?;
        READ_LEFT.with(|b| b.set(Some(MAX_READ)));
        let result = render(request.session);
        let reply = match result {
            Ok(preview) => Reply {
                nonce: request.nonce,
                preview: Some(preview),
                error: None,
            },
            Err(error) => Reply {
                nonce: request.nonce,
                preview: None,
                error: Some((error.code().into(), error.to_string())),
            },
        };
        let output = serde_json::to_vec(&reply)?;
        if output.len() > MAX_REPLY {
            return Err(Error::Limit("preview output".into()));
        }
        std::io::stdout().write_all(&output)?;
        Ok(())
    };
    Some(if run().is_ok() { 0 } else { 4 })
}

fn render(mut session: Session) -> Result<Preview> {
    if session.candidates.len() != 1 || !session.supported_version() {
        return Err(Error::Request("preview session".into()));
    }
    restore::validation::layout(&session)?;
    let id = session.candidates[0].id.clone();
    let mut old = session.candidates.remove(0);
    if session.filesystem == FileSystem::Disk {
        let volume = session
            .volumes
            .iter()
            .find(|v| old.id.starts_with(&format!("p{}:", v.region.index)))
            .ok_or(Error::SourceChanged)?;
        old.id = old
            .id
            .strip_prefix(&format!("p{}:", volume.region.index))
            .ok_or(Error::SourceChanged)?
            .into();
        session.filesystem = volume.region.filesystem.ok_or(Error::SourceChanged)?;
        session.region = Some(volume.region.clone());
        session.fingerprint = volume.fingerprint.clone();
        if let Some(info) = &mut session.source_info {
            info.offset = volume.region.offset;
            info.length = volume.region.length;
        }
        session.volumes.clear();
    }
    let extension = old
        .name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let image_type = matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "jpe");
    if !image_type
        && !matches!(
            extension.as_str(),
            "txt"
                | "md"
                | "csv"
                | "tsv"
                | "log"
                | "json"
                | "xml"
                | "yaml"
                | "yml"
                | "toml"
                | "ini"
                | "rs"
                | "py"
                | "c"
                | "cpp"
                | "h"
                | "js"
                | "ts"
                | "html"
                | "css"
                | "sql"
                | "sh"
                | "ps1"
                | "bat"
                | "tex"
        )
    {
        return Err(Error::Unsupported(
            "preview supports PNG, JPEG and plain-text files; recovery support is independent"
                .into(),
        ));
    }
    if image_type && old.size.is_none_or(|size| size > MAX_ENCODED) {
        return Err(Error::Limit("preview encoded image exceeds 8 MiB".into()));
    }
    let cancel = AtomicBool::new(false);
    let mut image = Image::open_region(&session.source, session.region.as_ref())?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    image.validate_raid(&session)?;
    if session.filesystem == FileSystem::Raw {
        // Carving's general recovery validator can decode a much larger image.
        // Revalidate the bounded original bytes first, then use only the preview
        // decoder, so forged saved size fields cannot bypass preview limits.
        let format = if old.id == format!("png-{:016x}", old.record) {
            "png"
        } else if old.id == format!("jpeg-{:016x}", old.record) {
            "jpeg"
        } else {
            return Err(Error::Unsupported(
                "preview carving supports PNG/JPEG".into(),
            ));
        };
        let size = old
            .size
            .filter(|size| *size > 0 && *size <= MAX_ENCODED)
            .ok_or_else(|| Error::Limit("preview encoded image size".into()))?;
        if old.sequence != 0 || old.stream_name.is_some() || old.fragment_offset.is_some() {
            return Err(Error::SourceChanged);
        }
        if let Some(settings) = &session.search {
            settings.validate()?;
            if !settings.matches(&old)
                || old.record < settings.range_start
                || old
                    .record
                    .checked_add(size)
                    .is_none_or(|end| end > settings.range_end.unwrap_or(image.length))
            {
                return Err(Error::SourceChanged);
            }
        }
        carve::validate_scope(&mut image, &session, &old, &cancel)?;
        let bytes = image.read(old.record, size as usize)?;
        if format!("{:x}", Sha256::digest(&bytes)) != old.record_sha256
            || image.fingerprint()? != session.fingerprint
        {
            return Err(Error::SourceChanged);
        }
        let mut preview = decode(&bytes, true, false)?;
        if preview.info.format != format {
            return Err(Error::SourceChanged);
        }
        preview.info.candidate_id = id;
        preview.info.source_bytes_read = READ_LEFT.with(|b| MAX_READ - b.get().unwrap_or(MAX_READ));
        preview.info.warnings = old.warnings;
        preview.validate()?;
        return Ok(preview);
    }
    let source = match session.filesystem {
        FileSystem::Ntfs => match Ntfs::open(&mut image) {
            Ok(ntfs) => RecoverySource::Ntfs(Box::new(ntfs)),
            Err(_) if old.fragment_offset.is_some() => RecoverySource::NtfsFragments,
            Err(e) => return Err(e),
        },
        FileSystem::Raw => RecoverySource::Carved,
        FileSystem::Fat12 | FileSystem::Fat16 | FileSystem::Fat32 | FileSystem::Exfat => {
            let fresh = restore::validation::fat_selection(
                &mut image,
                &BTreeSet::from([old.id.clone()]),
                session
                    .search
                    .as_ref()
                    .is_some_and(|s| s.estimate_fat_contiguous),
                session
                    .search
                    .as_ref()
                    .map_or(crate::FatShortNameEncoding::Ascii, |s| {
                        s.fat_short_name_encoding
                    }),
                session
                    .search
                    .as_ref()
                    .is_some_and(|s| s.exfat_allow_damaged_upcase),
                &cancel,
            )?;
            if fresh.filesystem != session.filesystem {
                return Err(Error::SourceChanged);
            }
            RecoverySource::Fat(
                fresh
                    .entries
                    .into_iter()
                    .map(|e| (e.candidate.id.clone(), e))
                    .collect(),
            )
        }
        FileSystem::Disk => return Err(Error::SourceChanged),
    };
    let candidate = restore::validation::candidate(
        &source,
        &mut image,
        &old,
        &mut indexed_scan::Parents::default(),
        &session,
        &cancel,
    )?;
    let data = candidate_data(&source, &mut image, &candidate, &cancel)?;
    let total = data.data.size();
    if image_type && total > MAX_ENCODED {
        return Err(Error::Limit("preview image exceeds 8 MiB".into()));
    }
    let count = if image_type {
        total as usize
    } else {
        total.min(MAX_TEXT as u64) as usize
    };
    let bytes = data.data.read(&mut image, 0, count)?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    let mut preview = decode(&bytes, image_type, total > count as u64)?;
    preview.info.candidate_id = id;
    preview.info.source_bytes = count as u64;
    preview.info.source_bytes_read = READ_LEFT.with(|b| MAX_READ - b.get().unwrap_or(MAX_READ));
    preview.info.warnings = data.warnings;
    preview.info.warnings.push(
        "Preview is derived display data, not a recovery or proof of original content.".into(),
    );
    preview.validate()?;
    Ok(preview)
}

fn decode(bytes: &[u8], image_type: bool, truncated: bool) -> Result<Preview> {
    let mut preview = Preview {
        info: PreviewInfo {
            schema_version: 1,
            candidate_id: String::new(),
            format: "text".into(),
            source_bytes: bytes.len() as u64,
            source_width: 0,
            source_height: 0,
            width: 0,
            height: 0,
            truncated,
            source_bytes_read: 0,
            warnings: Vec::new(),
        },
        rgba: Vec::new(),
        text: String::new(),
    };
    if !image_type {
        preview.text = text(bytes, truncated)?;
        return Ok(preview);
    }
    let (width, height, pixels) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        preview.info.format = "png".into();
        let mut decoder = png::Decoder::new_with_limits(
            Cursor::new(bytes),
            png::Limits {
                bytes: MAX_PIXELS * 4,
            },
        );
        decoder.set_ignore_text_chunk(true);
        decoder.set_ignore_iccp_chunk(true);
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder
            .read_info()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let (w, h) = (reader.info().width as usize, reader.info().height as usize);
        dimensions(w, h)?;
        if reader.info().animation_control.is_some() {
            return Err(Error::Unsupported("animated PNG preview".into()));
        }
        let size = reader
            .output_buffer_size()
            .filter(|n| *n <= MAX_PIXELS * 4)
            .ok_or_else(|| Error::Limit("preview PNG pixels".into()))?;
        let mut bytes = vec![0; size];
        let out = reader
            .next_frame(&mut bytes)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let channels = out.color_type.samples();
        let mut rgba = Vec::with_capacity(w * h * 4);
        for p in bytes[..out.buffer_size()].chunks_exact(channels) {
            match out.color_type {
                png::ColorType::Rgba => rgba.extend_from_slice(p),
                png::ColorType::Rgb => rgba.extend_from_slice(&[p[0], p[1], p[2], 255]),
                png::ColorType::Grayscale => rgba.extend_from_slice(&[p[0], p[0], p[0], 255]),
                png::ColorType::GrayscaleAlpha => rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]),
                _ => return Err(Error::Invalid("PNG palette expansion".into())),
            }
        }
        (w, h, rgba)
    } else if bytes.starts_with(b"\xff\xd8") {
        preview.info.format = "jpeg".into();
        use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
        let options = DecoderOptions::new_safe()
            .set_strict_mode(true)
            .set_use_unsafe(false)
            .set_max_width(65535)
            .set_max_height(65535)
            .jpeg_set_max_scans(128)
            .jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
        decoder
            .decode_headers()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let (w, h) = decoder
            .dimensions()
            .ok_or_else(|| Error::Invalid("JPEG dimensions".into()))?;
        dimensions(w, h)?;
        let size = decoder
            .output_buffer_size()
            .filter(|n| *n == w * h * 4)
            .ok_or_else(|| Error::Invalid("JPEG RGBA length".into()))?;
        let mut rgba = vec![0; size];
        decoder
            .decode_into(&mut rgba)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        (w, h, rgba)
    } else {
        return Err(Error::Unsupported("preview image signature".into()));
    };
    preview.info.source_width = width as u32;
    preview.info.source_height = height as u32;
    let side = width.max(height);
    let w = if side > 512 {
        (width * 512 / side).max(1)
    } else {
        width
    };
    let h = if side > 512 {
        (height * 512 / side).max(1)
    } else {
        height
    };
    preview.info.width = w as u32;
    preview.info.height = h as u32;
    preview.rgba.reserve(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let at = ((y * height / h) * width + x * width / w) * 4;
            preview.rgba.extend_from_slice(&pixels[at..at + 4]);
        }
    }
    Ok(preview)
}
fn dimensions(w: usize, h: usize) -> Result<()> {
    if w == 0 || h == 0 || w > 65535 || h > 65535 || w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS)
    {
        Err(Error::Limit(
            "preview image exceeds 16,777,216 pixels or 65,535 per dimension".into(),
        ))
    } else {
        Ok(())
    }
}
fn text(bytes: &[u8], truncated: bool) -> Result<String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if !truncated && !bytes.len().is_multiple_of(2) {
            return Err(Error::Unsupported("truncated UTF-16 code unit".into()));
        }
        let little = bytes[0] == 0xff;
        let mut words = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect::<Vec<_>>();
        if truncated && words.last().is_some_and(|x| (0xd800..=0xdbff).contains(x)) {
            words.pop();
        }
        String::from_utf16(&words)
            .map_err(|_| Error::Unsupported("preview text is not valid UTF-16".into()))?
    } else {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(text) => text.into(),
            Err(e) if truncated && e.error_len().is_none() => {
                std::str::from_utf8(&bytes[..e.valid_up_to()])
                    .map_err(|e| Error::Invalid(e.to_string()))?
                    .into()
            }
            Err(_) => {
                return Err(Error::Unsupported(
                    "preview text needs UTF-8 or BOM-marked UTF-16".into(),
                ));
            }
        }
    };
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(Error::Unsupported(
            "binary control bytes in text preview".into(),
        ));
    }
    Ok(text)
}

#[derive(Debug, Serialize)]
pub struct PreviewExport {
    pub preview: PreviewInfo,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes_written: usize,
}
/// Publishes a derived PNG/text with a unique name after the complete preview is
/// available. MCP supplies pinned roots; the worker never receives a write path.
pub fn save_preview(
    session: &Session,
    preview: &Preview,
    destination: &Path,
    roots: Option<&[OutputRoot]>,
) -> Result<PreviewExport> {
    preview.validate()?;
    session.guard_output(destination)?;
    let mut bytes = Vec::new();
    let extension = if preview.info.format == "text" {
        bytes.extend_from_slice(preview.text.as_bytes());
        "txt"
    } else {
        let mut encoder = png::Encoder::new(&mut bytes, preview.info.width, preview.info.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| Error::Invalid(e.to_string()))?;
        writer
            .write_image_data(&preview.rgba)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        writer.finish().map_err(|e| Error::Invalid(e.to_string()))?;
        "png"
    };
    let destination = match roots {
        Some(roots) => RecoveryDestination::within_create(destination, roots)?,
        None => RecoveryDestination::open(destination)?,
    };
    let name = format!("preview-{}.{}", Uuid::new_v4(), extension);
    destination.verify()?;
    let mut file = destination
        .dir
        .open_with(&name, OpenOptions::new().write(true).create_new(true))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    destination.verify()?;
    Ok(PreviewExport {
        preview: preview.info.clone(),
        path: destination.path.join(name),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes_written: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_is_literal_bounded_and_encoding_explicit() {
        assert_eq!(
            text(b"<script>alert('x')</script>", false).unwrap(),
            "<script>alert('x')</script>"
        );
        assert_eq!(text(&[0xff, 0xfe, 0x42, 0x30], false).unwrap(), "あ");
        assert_eq!(text(b"ok\xe3\x81", true).unwrap(), "ok");
        assert!(text(b"ok\xe3\x81", false).is_err());
        assert!(text(b"a\0b", false).is_err());
        assert!(text(b"\xff", true).is_err());
    }
    #[test]
    fn pixels_and_read_budget_are_independent() {
        assert!(dimensions(65535, 65535).is_err());
        assert!(dimensions(0, 1).is_err());
        READ_LEFT.with(|b| b.set(Some(9)));
        assert!(charge_read(8).is_ok());
        assert!(charge_read(2).is_err());
        assert!(charge_read(1).is_ok());
        READ_LEFT.with(|b| b.set(None));
    }
}
