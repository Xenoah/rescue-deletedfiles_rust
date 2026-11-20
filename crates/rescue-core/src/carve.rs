//! Bounded contiguous image/document/media carving. Deletion and allocation remain unknown.
mod aac;
mod aiff;
mod asf;
mod avi;
mod cab;
mod compressed;
mod container;
mod ebml;
mod extra;
mod flac;
mod font;
mod frame_archive;
mod icon;
mod indexed;
mod rar;
mod rtf;
mod rule;
mod scope;
mod sevenz;
mod streams;
mod tar;
mod tiff;
pub(crate) use indexed::{resume as resume_indexed, scan as scan_indexed};
pub use rule::SignatureRule;
pub(crate) use scope::validate_selection as validate_scope;
mod bmff;
mod heif;
mod html;
mod jpeg;
mod jpeg2000;
mod json;
mod movie;
mod mp3;
mod netpbm;
mod openexr;
mod pdf;
mod photoshop;
mod png;
mod xml;
mod zip;
use crate::{
    AllocationStatus, Candidate, Error, ExtentEvidence, FileSystem, Progress, ProgressUnit,
    RecoveryData, Result, ScanOptions, Session, ntfs::Data, source::Image,
};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const WINDOW: u64 = 1024 * 1024;
// Three consecutive ADTS headers can span two maximum-length frames.
const SIGNATURE_LOOKAHEAD: u64 = aac::LOOKAHEAD;
const VALIDATION_BUDGET: u64 = 4 * 1024 * 1024 * 1024;
const PROVENANCE: &str = "Contiguous carving: original name, path and deletion status are unknown. Allocation status describes the current allocation map only; it does not prove deletion or original content. Live files and embedded images may also be found.";

/// Reject failed structure checks on an explicitly estimated FAT extent. A
/// successful structure check still cannot prove original file identity.
pub(crate) fn validate_estimated_extent(
    image: &mut Image,
    at: u64,
    size: u64,
    name: &str,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<bool> {
    if size > container::MAX_ENCODED as u64 {
        return Err(Error::Limit(
            "estimated FAT file exceeds the 64 MiB validation limit".into(),
        ));
    }
    let end = at
        .checked_add(size)
        .filter(|end| *end <= image.length)
        .ok_or_else(|| Error::Invalid("estimated FAT extent outside source".into()))?;
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let head = image.read(at, size.min(12) as usize)?;
    charge(budget, head.len())?;
    let format = match extension.as_str() {
        "png" | "apng" => Some(Format::Png),
        "jpg" | "jpeg" | "jpe" => Some(Format::Jpeg),
        "pdf" => Some(Format::Pdf),
        "jp2" => Some(Format::Jp2),
        "j2k" | "j2c" | "jpc" => Some(Format::J2k),
        "exr" => Some(Format::Exr),
        "psd" => Some(Format::Psd),
        "psb" => Some(Format::Psb),
        "tif" | "tiff" => Some(Format::Tiff),
        "ttf" => Some(Format::Ttf),
        "otf" if head.starts_with(b"PK\x03\x04") => Some(Format::Zip),
        "otf" => Some(Format::Otf),
        "woff" => Some(Format::Woff),
        "flac" => Some(Format::Flac),
        "aif" | "aiff" => Some(aiff::signature(&head).unwrap_or(Format::Aiff)),
        "aifc" | "afc" => Some(Format::Aifc),
        "asf" => Some(Format::Asf),
        "wma" => Some(Format::Wma),
        "wmv" => Some(Format::Wmv),
        "aac" | "adts" => Some(Format::Aac),
        "mp3" => Some(Format::Mp3),
        "pbm" => Some(Format::Pbm),
        "pgm" => Some(Format::Pgm),
        "ppm" => Some(Format::Ppm),
        "pam" => Some(Format::Pam),
        "pnm" => Some(
            netpbm::signature(&head)
                .ok_or_else(|| Error::Invalid("missing Netpbm magic".into()))?,
        ),
        "html" | "htm" => Some(Format::Html),
        "xml" => Some(Format::Xml),
        "svg" => Some(Format::Svg),
        "svgz" => Some(Format::Svgz),
        "json" => Some(Format::Json),
        "rtf" => Some(Format::Rtf),
        "zip" | "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" => Some(Format::Zip),
        "heic" | "heif" => Some(Format::Heic),
        "avif" => Some(Format::Avif),
        "mp4" | "mov" | "m4v" | "m4a" => Some(Format::Mp4),
        "wav" => Some(Format::Wav),
        "webp" => Some(Format::Webp),
        "bmp" => Some(Format::Bmp),
        "gif" => Some(Format::Gif),
        "avi" => Some(Format::Avi),
        "mkv" => Some(Format::Mkv),
        "webm" => Some(Format::Webm),
        "ogg" => Some(Format::Ogg),
        "rar" => Some(Format::Rar),
        "7z" => Some(Format::SevenZ),
        "cab" => Some(Format::Cab),
        "gz" => Some(Format::Gzip),
        "ico" => Some(Format::Ico),
        "cur" => Some(Format::Cur),
        "zst" => Some(Format::Zstd),
        "lz4" => Some(Format::Lz4),
        "bz2" => Some(Format::Bzip),
        "xz" => Some(Format::Xz),
        "lz" => Some(Format::Lzip),
        "sqlite" => Some(Format::Sqlite),
        ext if zip::package::formats().any(|kind| kind.extension() == ext) => Some(Format::Zip),
        _ if head.starts_with(SIGNATURE) => Some(Format::Png),
        _ if head.starts_with(b"\xff\xd8\xff") => Some(Format::Jpeg),
        _ if head.starts_with(b"%PDF-") => Some(Format::Pdf),
        _ if head.starts_with(b"PK\x03\x04") => Some(Format::Zip),
        _ => None,
    };
    let Some(format) = format else {
        return Ok(false);
    };
    if format == Format::Svg {
        if size > xml::MAX_BYTES as u64 {
            return Err(Error::Limit("SVG XML exceeds 8 MiB".into()));
        }
        let mut input = container::Input::new(image, at, end, cancel, budget)?;
        input.ensure(size as usize)?;
        input.work(size as usize)?;
        if !xml::complete_svg(&input.bytes[..size as usize]) {
            return Err(Error::Invalid(
                "invalid SVG document in estimated extent".into(),
            ));
        }
        return Ok(true);
    }
    if matches!(
        format,
        Format::Html | Format::Pbm | Format::Pgm | Format::Ppm | Format::Pam
    ) {
        let mut input = container::Input::new(image, at, end, cancel, budget)?;
        let length = if format == Format::Html {
            html::parse(&mut input, true)?.0
        } else {
            netpbm::parse(format, &mut input, true)?
        };
        if length as u64 != size {
            return Err(Error::Invalid(
                "estimated FAT length differs from validated document or image extent".into(),
            ));
        }
        return Ok(true);
    }
    let (actual, validated, _) = format.validate(image, at, cancel, budget, end)?;
    if matches!(format, Format::Wma | Format::Wmv) && actual != format {
        return Err(Error::Invalid(
            "ASF streams differ from estimated file type".into(),
        ));
    }
    if validated.len() as u64 > size {
        return Err(Error::Invalid(
            "validated format exceeds estimated FAT size".into(),
        ));
    }
    if validated.len() as u64 != size {
        let remaining = size.saturating_sub(validated.len() as u64);
        if format != Format::Pdf || remaining > 4096 {
            return Err(Error::Invalid(
                "estimated FAT length differs from validated format extent".into(),
            ));
        }
        let tail = image.read(at + validated.len() as u64, remaining as usize)?;
        charge(budget, tail.len())?;
        if !tail.iter().all(u8::is_ascii_whitespace) {
            return Err(Error::Invalid(
                "non-whitespace after estimated PDF extent".into(),
            ));
        }
    }
    Ok(true)
}

pub(crate) fn is_open_package_id(id: &str) -> bool {
    id.split_once('-')
        .is_some_and(|(ext, _)| zip::package::formats().any(|format| format.extension() == ext))
}

fn charge(budget: &mut u64, amount: usize) -> Result<()> {
    *budget = budget
        .checked_sub(amount as u64)
        .ok_or_else(|| Error::Limit("carving scan validation budget exhausted".into()))?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format<'a> {
    Rule(&'a SignatureRule),
    Png,
    Apng,
    Jpeg,
    Jp2,
    J2k,
    Tiff,
    Exr,
    Psd,
    Psb,
    Aiff,
    Aifc,
    Flac,
    Ttf,
    Otf,
    Woff,
    Mp3,
    Aac,
    Asf,
    Wma,
    Wmv,
    Tar(u16),
    Pbm,
    Pgm,
    Ppm,
    Pam,
    Html,
    Xml,
    Svg,
    Svgz,
    Json,
    Rtf,
    Pdf,
    Zip,
    Package(&'static str),
    Docx,
    Xlsx,
    Pptx,
    Heic,
    Avif,
    Mp4,
    Mov,
    Wav,
    Webp,
    Bmp,
    Gif,
    Avi,
    Mkv,
    Webm,
    Ogg,
    Gzip,
    Rar,
    SevenZ,
    Cab,
    Ico,
    Cur,
    Zstd,
    Lz4,
    Bzip,
    Xz,
    Lzip,
    Sqlite,
}
impl<'a> Format<'a> {
    fn prefix(self) -> &'static str {
        match self {
            Self::Rule(_) => "rule",
            Self::Png => "png",
            Self::Apng => "apng",
            Self::Jpeg => "jpeg",
            Self::Tiff => "tif",
            Self::Jp2 => "jp2",
            Self::J2k => "j2k",
            Self::Exr => "exr",
            Self::Psd => "psd",
            Self::Psb => "psb",
            Self::Aiff => "aiff",
            Self::Aifc => "aifc",
            Self::Flac => "flac",
            Self::Ttf => "ttf",
            // "otf" already identifies OpenDocument formula-template sessions.
            Self::Otf => "opentype",
            Self::Woff => "woff",
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::Tar(_) => "tar",
            Self::Pbm => "pbm",
            Self::Pgm => "pgm",
            Self::Ppm => "ppm",
            Self::Pam => "pam",
            Self::Html => "html",
            Self::Xml => "xml",
            Self::Svg => "svg",
            Self::Svgz => "svgz",
            Self::Json => "json",
            Self::Rtf => "rtf",
            Self::Pdf => "pdf",
            Self::Zip => "zip",
            Self::Package(ext) => ext,
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Heic => "heic",
            Self::Avif => "avif",
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Wav => "wav",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Gif => "gif",
            Self::Avi => "avi",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Ogg => "ogg",
            Self::Gzip => "gz",
            Self::Rar => "rar",
            Self::Asf => "asf",
            Self::Wma => "wma",
            Self::Wmv => "wmv",
            Self::SevenZ => "7z",
            Self::Cab => "cab",
            Self::Ico => "ico",
            Self::Cur => "cur",
            Self::Zstd => "zst",
            Self::Lz4 => "lz4",
            Self::Bzip => "bz2",
            Self::Xz => "xz",
            Self::Lzip => "lz",
            Self::Sqlite => "sqlite",
        }
    }
    fn extension(self) -> &'a str {
        match self {
            Self::Rule(rule) => &rule.extension,
            Self::Jpeg => "jpg",
            Self::Otf => "otf",
            other => other.prefix(),
        }
    }
    fn id(self, offset: u64) -> String {
        if let Self::Tar(n) = self
            && n != 1
        {
            return format!("tar-{offset:016x}-b{n}");
        }
        if let Self::Rule(rule) = self {
            return format!(
                "rule-{offset:016x}-{:x}",
                Sha256::digest(rule.to_string().as_bytes())
            );
        }
        format!("{}-{offset:016x}", self.prefix())
    }
    fn name(self, offset: u64) -> String {
        format!("{}_{offset:016x}.{}", self.prefix(), self.extension())
    }
    fn evidence(self) -> ExtentEvidence {
        match self {
            Self::Zstd | Self::Lz4 | Self::Bzip | Self::Xz | Self::Lzip => {
                ExtentEvidence::FileStructure
            }
            Self::Rule(_) => ExtentEvidence::SignatureRule,
            Self::Png | Self::Apng => ExtentEvidence::PngValidated,
            Self::Jpeg => ExtentEvidence::JpegValidated,
            Self::Pdf => ExtentEvidence::PdfStructure,
            Self::Zip | Self::Docx | Self::Xlsx | Self::Pptx | Self::Package(_) => {
                ExtentEvidence::ZipCrcValidated
            }
            Self::Heic | Self::Avif | Self::Mp4 | Self::Mov => ExtentEvidence::MediaStructure,
            Self::Tiff
            | Self::Jp2
            | Self::J2k
            | Self::Exr
            | Self::Psd
            | Self::Psb
            | Self::Aiff
            | Self::Aifc
            | Self::Flac
            | Self::Ttf
            | Self::Otf
            | Self::Woff
            | Self::Mp3
            | Self::Aac
            | Self::Tar(_)
            | Self::Pbm
            | Self::Pgm
            | Self::Ppm
            | Self::Pam
            | Self::Html
            | Self::Xml
            | Self::Svg
            | Self::Svgz
            | Self::Json
            | Self::Rtf
            | Self::Ico
            | Self::Cur
            | Self::Avi
            | Self::Mkv
            | Self::Webm
            | Self::Ogg
            | Self::Cab
            | Self::Asf
            | Self::Wmv
            | Self::Wma
            | Self::Rar
            | Self::SevenZ
            | Self::Gzip
            | Self::Wav
            | Self::Webp
            | Self::Bmp
            | Self::Gif
            | Self::Sqlite => ExtentEvidence::FileStructure,
        }
    }
    fn allowed(self, extensions: &[String]) -> bool {
        extensions.is_empty()
            || extensions.iter().any(|ext| {
                let ext = ext
                    .trim()
                    .trim_start_matches("*.")
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                ext == "*"
                    || match self {
                        Self::Pbm | Self::Pgm | Self::Ppm | Self::Pam => {
                            ext == "pnm" || ext == self.extension()
                        }
                        Self::J2k => matches!(ext.as_str(), "j2k" | "j2c" | "jpc"),
                        Self::Html => matches!(ext.as_str(), "html" | "htm"),
                        Self::Aac => matches!(ext.as_str(), "aac" | "adts"),
                        Self::Aiff => matches!(ext.as_str(), "aif" | "aiff"),
                        Self::Aifc => matches!(ext.as_str(), "afc" | "aifc" | "aif" | "aiff"),
                        Self::Tiff => matches!(ext.as_str(), "tif" | "tiff"),
                        Self::Png => ext == "png",
                        Self::Apng => matches!(ext.as_str(), "apng" | "png"),
                        Self::Jpeg => matches!(ext.as_str(), "jpg" | "jpeg" | "jpe"),
                        Self::Heic => matches!(ext.as_str(), "heic" | "heif"),
                        Self::Package(subtype) => ext == subtype || ext == "zip",
                        other => ext == other.extension(),
                    }
            })
    }
    // Preserve a requested generic container extension when its subtype is filtered out.
    fn for_extensions(self, extensions: &[String]) -> Self {
        match self {
            Self::Wmv | Self::Wma if !self.allowed(extensions) && Self::Asf.allowed(extensions) => {
                Self::Asf
            }
            Self::Svg if !self.allowed(extensions) && Self::Xml.allowed(extensions) => Self::Xml,
            Self::Svgz if !self.allowed(extensions) && Self::Gzip.allowed(extensions) => Self::Gzip,
            other => other,
        }
    }
    fn validate(
        self,
        image: &mut Image,
        at: u64,
        cancel: &AtomicBool,
        budget: &mut u64,
        boundary: u64,
    ) -> Result<(Format<'a>, Vec<u8>, Vec<usize>)> {
        match self {
            Self::Zstd | Self::Lz4 => {
                frame_archive::validate(self, image, at, cancel, budget, boundary)
                    .map(|b| (self, b, Vec::new()))
            }
            Self::Bzip | Self::Xz | Self::Lzip => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = compressed::parse(self, &mut input)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Pbm | Self::Pgm | Self::Ppm | Self::Pam => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = netpbm::parse(self, &mut input, false)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Xml | Self::Svg => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let (end, internal, svg) = xml::parse(&mut input, self == Self::Svg)?;
                Ok((
                    if svg { Self::Svg } else { Self::Xml },
                    input.finish(end)?,
                    internal,
                ))
            }
            Self::Html | Self::Mp3 => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let (end, internal) = match self {
                    Self::Html => html::parse(&mut input, false)?,
                    _ => mp3::parse(&mut input)?,
                };
                Ok((self, input.finish(end)?, internal))
            }
            Self::Cab => {
                cab::validate(image, at, boundary, cancel, budget).map(|b| (self, b, Vec::new()))
            }
            Self::Asf | Self::Wma | Self::Wmv => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let (kind, end) = asf::parse(&mut input)?;
                Ok((kind, input.finish(end)?, Vec::new()))
            }
            Self::Rar => {
                rar::validate(image, at, boundary, cancel, budget).map(|b| (self, b, Vec::new()))
            }
            Self::SevenZ => {
                sevenz::validate(image, at, boundary, cancel, budget).map(|b| (self, b, Vec::new()))
            }
            Self::Tar(factor) => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let (end, records) = tar::parse(&mut input, factor)?;
                Ok((self, input.finish(end)?, records))
            }
            Self::Aac => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = aac::parse(&mut input)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Json => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = json::parse(&mut input)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Jp2 | Self::J2k => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let (end, inner) = jpeg2000::parse(&mut input, self)?;
                Ok((self, input.finish(end)?, inner.into_iter().collect()))
            }
            Self::Exr => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = openexr::parse(&mut input)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Psd | Self::Psb => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = photoshop::parse(&mut input, self)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Tiff => {
                tiff::validate(image, at, cancel, budget, boundary).map(|b| (self, b, Vec::new()))
            }
            Self::Ttf | Self::Otf | Self::Woff => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = font::parse(&mut input, self)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Aiff | Self::Aifc => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = aiff::parse(&mut input, self)?;
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Rtf | Self::Flac => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let end = if self == Self::Rtf {
                    rtf::parse(&mut input)?
                } else {
                    flac::parse(&mut input)?
                };
                Ok((self, input.finish(end)?, Vec::new()))
            }
            Self::Ico | Self::Cur => icon::validate(self, image, at, cancel, budget, boundary)
                .map(|b| (self, b, Vec::new())),
            Self::Avi | Self::Mkv | Self::Webm | Self::Ogg | Self::Gzip | Self::Svgz => {
                let mut input = container::Input::new(image, at, boundary, cancel, budget)?;
                let (format, end) = match self {
                    Self::Avi => (self, avi::parse(&mut input)?),
                    Self::Ogg => (self, streams::ogg(&mut input)?),
                    Self::Gzip | Self::Svgz => {
                        let (end, svg) = streams::gzip(&mut input)?;
                        if self == Self::Svgz && !svg {
                            return Err(Error::Invalid("gzip payload is not supported SVG".into()));
                        }
                        (if svg { Self::Svgz } else { Self::Gzip }, end)
                    }
                    _ => ebml::parse(&mut input)?,
                };
                Ok((format, input.finish(end)?, Vec::new()))
            }
            Self::Wav | Self::Webp | Self::Bmp | Self::Gif | Self::Sqlite => {
                extra::validate(self, image, at, cancel, budget, boundary)
                    .map(|b| (self, b, Vec::new()))
            }
            Self::Rule(rule) => rule
                .extract(image, at, cancel, budget, boundary)
                .map(|b| (self, b, Vec::new())),
            Self::Png | Self::Apng => png::validate(image, at, cancel, budget, boundary)
                .map(|(format, b)| (format, b, Vec::new())),
            Self::Jpeg => {
                jpeg::validate(image, at, cancel, budget, boundary).map(|b| (self, b, Vec::new()))
            }
            Self::Pdf => {
                pdf::validate(image, at, cancel, budget, boundary).map(|b| (self, b, Vec::new()))
            }
            Self::Zip | Self::Docx | Self::Xlsx | Self::Pptx | Self::Package(_) => {
                zip::validate(image, at, cancel, budget, boundary)
            }
            Self::Avif | Self::Heic => heif::validate(self, image, at, cancel, budget, boundary)
                .map(|bytes| (self, bytes, Vec::new())),
            Self::Mp4 | Self::Mov => movie::validate(image, at, cancel, budget, boundary)
                .map(|(format, b)| (format, b, Vec::new())),
        }
    }
}

// Built-in signatures all require a nonzero byte within the detector's lookahead.
// Keep that overlap for signatures such as ISO BMFF and tar whose magic follows the start.
fn signature_offsets(
    buffer: &[u8],
    count: usize,
    rule: Option<&SignatureRule>,
) -> std::ops::Range<usize> {
    if rule.is_some() {
        return 0..count;
    }
    let Some(first) = buffer.iter().position(|&byte| byte != 0) else {
        return 0..0;
    };
    let last = buffer.iter().rposition(|&byte| byte != 0).unwrap();
    first.saturating_sub(SIGNATURE_LOOKAHEAD as usize)..count.min(last + 1)
}

fn detect<'a>(
    bytes: &[u8],
    extensions: &[String],
    rule: Option<&'a SignatureRule>,
    tar_factor: Option<u16>,
) -> Option<Format<'a>> {
    if let Some(rule) = rule {
        return bytes
            .starts_with(&rule.header)
            .then_some(Format::Rule(rule))
            .filter(|format| format.allowed(extensions));
    }
    if tar::signature(bytes) && Format::Tar(1).allowed(extensions) {
        Some(Format::Tar(tar_factor.unwrap_or(1)))
    } else if bytes.starts_with(b"P")
        && let Some(format) = netpbm::signature(bytes)
    {
        Some(format).filter(|f| f.allowed(extensions))
    } else if bytes.starts_with(openexr::MAGIC) {
        Format::Exr.allowed(extensions).then_some(Format::Exr)
    } else if bytes.starts_with(jpeg2000::BOX_SIGNATURE) {
        Format::Jp2.allowed(extensions).then_some(Format::Jp2)
    } else if bytes.starts_with(jpeg2000::CODESTREAM_SIGNATURE) {
        Format::J2k.allowed(extensions).then_some(Format::J2k)
    } else if bytes.starts_with(SIGNATURE) {
        [Format::Png, Format::Apng]
            .into_iter()
            .find(|f| f.allowed(extensions))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) && Format::Jpeg.allowed(extensions) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"{\\rtf1") && Format::Rtf.allowed(extensions) {
        Some(Format::Rtf)
    } else if matches!(bytes.first(), Some(b'{' | b'[' | 0xef))
        && Format::Json.allowed(extensions)
        && json::signature(
            bytes,
            extensions.iter().any(|v| {
                v.trim()
                    .trim_start_matches("*.")
                    .trim_start_matches('.')
                    .eq_ignore_ascii_case("json")
            }),
        )
    {
        Some(Format::Json)
    } else if matches!(bytes.first(), Some(0x3c | 0xef))
        && html::signature(bytes)
        && Format::Html.allowed(extensions)
    {
        Some(Format::Html)
    } else if matches!(bytes.first(), Some(0x3c | 0xef))
        && xml::signature(bytes)
        && (Format::Xml.allowed(extensions) || Format::Svg.allowed(extensions))
    {
        Some(if Format::Xml.allowed(extensions) {
            Format::Xml
        } else {
            Format::Svg
        })
    } else if ((matches!(bytes.first(), Some(b'<' | 0xef)) && xml::svg_signature(bytes))
        || (matches!(bytes.first(), Some(0xfe | 0xff)) && xml::utf16_signature(bytes)))
        && Format::Svg.allowed(extensions)
    {
        Some(Format::Svg)
    } else if bytes.starts_with(&[0x1f, 0x8b, 8])
        && Format::Svgz.allowed(extensions)
        && !Format::Gzip.allowed(extensions)
    {
        Some(Format::Svgz)
    } else if bytes.first() == Some(&0xff)
        && aac::signature(bytes)
        && Format::Aac.allowed(extensions)
    {
        Some(Format::Aac)
    } else if matches!(bytes.first(), Some(0xff | b'I'))
        && mp3::signature(bytes)
        && Format::Mp3.allowed(extensions)
    {
        Some(Format::Mp3)
    } else if matches!(bytes.first(), Some(b'I' | b'M'))
        && tiff::signature(bytes)
        && Format::Tiff.allowed(extensions)
    {
        Some(Format::Tiff)
    } else if matches!(bytes.first(), Some(0 | b'O' | b't' | b'w'))
        && let Some(format) = font::signature(bytes)
        && format.allowed(extensions)
    {
        Some(format)
    } else if bytes.first() == Some(&b'8')
        && let Some(format) = photoshop::signature(bytes)
    {
        Some(format).filter(|f| f.allowed(extensions))
    } else if bytes.starts_with(b"fLaC") && Format::Flac.allowed(extensions) {
        Some(Format::Flac)
    } else if bytes.starts_with(b"MSCF") && Format::Cab.allowed(extensions) {
        Some(Format::Cab)
    } else if bytes.starts_with(&asf::MAGIC) {
        [Format::Asf, Format::Wma, Format::Wmv]
            .into_iter()
            .find(|f| f.allowed(extensions))
    } else if rar::signature(bytes) && Format::Rar.allowed(extensions) {
        Some(Format::Rar)
    } else if bytes.starts_with(sevenz::MAGIC) && Format::SevenZ.allowed(extensions) {
        Some(Format::SevenZ)
    } else if bytes.starts_with(b"%PDF-") && Format::Pdf.allowed(extensions) {
        Some(Format::Pdf)
    } else if (bytes.starts_with(b"PK\x03\x04")
        || (bytes.len() >= 22 && bytes.starts_with(b"PK\x05\x06") && bytes[4..20] == [0; 16]))
        && [Format::Zip, Format::Docx, Format::Xlsx, Format::Pptx]
            .into_iter()
            .chain(zip::package::formats())
            .any(|f| f.allowed(extensions))
    {
        Some(Format::Zip)
    } else if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        if bytes.get(8..12) == Some(b"jp2 ") {
            return None;
        }
        if let Some(still) = heif::signature(bytes) {
            still.allowed(extensions).then_some(still)
        } else {
            (Format::Mp4.allowed(extensions) || Format::Mov.allowed(extensions))
                .then_some(Format::Mp4)
        }
    } else {
        if bytes.starts_with(b"\x1a\x45\xdf\xa3") {
            return [Format::Mkv, Format::Webm]
                .into_iter()
                .find(|f| f.allowed(extensions));
        }
        if bytes.first() == Some(&0) {
            return icon::signature(bytes).filter(|f| f.allowed(extensions));
        }
        aiff::signature(bytes)
            .or_else(|| extra::signature(bytes))
            .or_else(|| compressed::signature(bytes))
            .or_else(|| frame_archive::signature(bytes))
            .filter(|format| format.allowed(extensions))
    }
}

fn format_warnings(format: Format) -> &'static [&'static str] {
    match format {
        Format::Rule(_) => &[
            "Extent estimated from a user-defined signature rule. The first matching footer or fixed length may include unrelated data or cut a file short. Original file type and content unverified.",
        ],
        Format::Jp2 | Format::J2k => &[
            "JPEG 2000 Part 1 headers, tile lengths, EOC and full image decoding checked. Entropy data has no whole-file checksum. JP2 includes contiguous recognized metadata boxes after the codestream.",
        ],
        Format::Exr => &[
            "OpenEXR header, channel layout, offset table and every pixel block decoded. Original names, deletion and file identity require independent evidence.",
        ],
        Format::Psd | Format::Psb => &[
            "Photoshop header, resource and layer section bounds, and composite channel lengths were checked. Layer effects and individual channel pixels are not rendered. Raw and PackBits data carry no independent content checksum.",
        ],
        Format::Tiff => &[
            "Classic TIFF/BigTIFF through the last referenced byte. Each supported page is pixel-decoded. Private pointer tags, subsidiary IFDs and unreferenced trailing padding are excluded. Up to 256 pages and 128 MiB decoded across all pages.",
        ],
        Format::Aiff | Format::Aifc => &[
            "FORM and padded chunk boundaries, sample rate representation, channel count, sample encoding and sound data length checked. PCM, 32/64-bit float and A-law/mu-law supported. Sample values have no embedded checksum.",
        ],
        Format::Ttf | Format::Otf | Format::Woff => &[
            "Standalone outline font. Directory, aligned extents, table checksums, metrics, character map and outline tables checked. SFNT also checks the entire file checksum and WOFF checks zlib streams. Rendering and every individual glyph are unverified.",
        ],
        Format::Flac => &[
            "Native FLAC with known sample count and explicit 8/12/16/20/24-bit frame depth. Frames are decoded with header and frame CRC checks; the PCM MD5 is compared when present. Metadata block boundaries are checked, metadata contents are not interpreted. Decoded i32 samples limited to 128 MiB.",
        ],
        Format::Cab => &[
            "Validated standalone cabinet layout, CFDATA checksums, and decoded folder/member lengths. Original file name and deletion status are unknown.",
        ],
        Format::Asf | Format::Wma | Format::Wmv => &[
            "Finite ASF container: file and stream properties, fixed packet and payload boundaries, and simple index references checked. Media content has no verified checksum.",
        ],
        Format::Rar => &[
            "RAR archive headers, end record and decoded member checksums verified. The original archive name and deletion status are unknown.",
        ],
        Format::SevenZ => &[
            "The contiguous 7z start and next headers and the decoded stream entries were CRC-checked. This does not establish deletion or the original filename.",
        ],
        Format::Tar(_) => &[
            "Member header checksums, stored lengths, padding and two terminating blocks were checked. TAR has no payload checksum. The blocking factor determines trailing record padding; files with different writer settings may have different byte lengths.",
        ],
        Format::Aac => &[
            "A contiguous run of at least three unprotected mono/stereo AAC-LC ADTS frames, with every payload decoded. ADTS has no file length or terminal marker. Adjacent streams with matching audio parameters can merge, and complete surviving frames may represent only part of a file. External tags are excluded.",
        ],
        Format::Mp3 => &[
            "MPEG Layer III with Xing/Info audio-frame and byte counts. Optional leading ID3v2 metadata retained. MPEG headers, frame lengths, sampling rate, channel count and declared end are checked. No audio decoding or trailing ID3v1/APE tag recovery.",
        ],
        Format::Json => &[
            "UTF-8 JSON object or array validated through its closing delimiter. The value may be embedded in a larger document. Leading and trailing whitespace outside the value is excluded, except whitespace following a UTF-8 BOM. No scalar-root or JSON-with-comments recovery.",
        ],
        Format::Pbm | Format::Pgm | Format::Ppm | Format::Pam => &[
            "Netpbm dimensions, channels, declared raster length and sample range checked. Plain P1–P3 include up to 64 trailing spaces/tabs and one final line ending. Each image is a separate candidate. Pixel data has no checksum.",
        ],
        Format::Html => &[
            "UTF-8 HTML5 doctype and balanced HTML tokens checked. An explicit </html> ends at the closing tag. A document with omitted html tags needs a closed element and zero padding or source EOF. No original checksum. External resources and scripts are never opened or executed.",
        ],
        Format::Svg => &[
            "SVG root in the W3C SVG namespace. UTF-8 or BOM-tagged UTF-16 XML structure validated through its root end. Trailing whitespace, comments and processing instructions have no independent file boundary. Image appearance and linked resources are unverified.",
        ],
        Format::Svgz => &[
            "A complete gzip member containing UTF-8 or BOM-tagged UTF-16 SVG. Member CRC32 and decoded length, XML structure and SVG namespace verified; external resources are never fetched.",
        ],
        Format::Xml => &[
            "Declared UTF-8 XML 1.0 through its matching root end tag. XML names, namespaces, attributes and predefined/numeric entities are validated. DTDs are excluded. Trailing comments, processing instructions and whitespace have no independent boundary and are excluded. 8 MiB and 65536 nodes per document.",
        ],
        Format::Rtf => &[
            "RTF extent ends at the matching outer group. Control words, hexadecimal escapes, nested groups and counted binary payloads are checked. Document rendering and bytes after the closing brace are unverified.",
        ],
        Format::Pdf => &[
            "Classic-xref PDF: references, page tree and declared stream extents validated. Encrypted/xref-stream PDFs are unsupported. Stream codecs, scripts and page rendering are not executed. EOF-based extent is an estimate; trailing padding cannot be established.",
        ],
        Format::Package(_) => &[
            "ZIP entries and every entry CRC checked. The stored leading MIME type and package manifest identify the document family. Document rendering, embedded objects and encryption inside the package are unverified.",
        ],
        Format::Zip | Format::Docx | Format::Xlsx | Format::Pptx => &[
            "ZIP central/local records, decoded lengths and every entry CRC validated. Office classification identifies a container, not document rendering or macro safety. Stored/DEFLATE only, no encryption/multi-volume; 4096 entries, 128 MiB decoded total, 64 MiB per entry.",
        ],
        Format::Heic | Format::Avif => &[
            "Item-based HEIC/AVIF still image. Local item locations, codec framing, linked images and bounded media payload were checked. Extent ends at the referenced mdat box; optional trailing boxes and original identity require independent evidence. Pixels are not decoded.",
        ],
        Format::Mp4 | Format::Mov => &[
            "Non-fragmented MP4/QuickTime container, complete sample tables and mdat ranges validated. Encoded video/audio frames are not decoded; original content is unknown. ftyp required, no zero-sized boxes, 1048576 samples, 256 tracks.",
        ],
        Format::Ico | Format::Cur => &[
            "ICO/CUR directory and contiguous resources checked. Embedded PNG pixels are decoded. Classic uncompressed DIB color data and AND mask must fit the recorded dimensions.",
        ],
        Format::Zstd | Format::Lz4 => &[
            "One complete Zstandard or LZ4 frame; decoded content length and block/content checksums are checked when present. Dictionary-dependent frames and skippable frames are excluded. Adjacent frames are separate candidates. 128 MiB decoded cap and 64 MiB Zstandard window cap.",
        ],
        Format::Bzip | Format::Xz | Format::Lzip => &[
            "Extent covers one decoded stream/member. Container checksums are validated when present. Adjacent members are separate candidates; optional XZ padding is excluded. Decoder memory limit for XZ/lzip is 64 MiB; decoded output limit is 128 MiB.",
        ],
        Format::Ogg => &[
            "Each candidate covers one Ogg logical stream from BOS to EOS. Page CRCs, serial, sequence and packet lacing verified. Chained streams are reported individually; interleaving is unsupported.",
        ],
        Format::Gzip => &[
            "Each candidate covers one gzip member. DEFLATE output CRC and length checked. Adjacent members are reported separately.",
        ],
        Format::Mkv | Format::Webm => &[
            "Declared-size EBML segment, track headers and cluster structure checked. Unknown-size live streams are unsupported. Media frames are not decoded.",
        ],
        Format::Avi => &[
            "Single classic RIFF AVI with bounded header, stream and media chunks. OpenDML indexes and multi-RIFF streams are unsupported. Frames are not decoded.",
        ],
        Format::Wav | Format::Webp | Format::Bmp | Format::Gif | Format::Sqlite => &[
            "Contiguous extent determined from format structure. Payload decoding, database integrity and original content are unverified.",
        ],
        _ => &[],
    }
}

/// Scans every byte of a regular image. `max_records` bounds signature attempts;
/// progress counters count source bytes, including the final signature overlap.
pub fn scan_carving(
    path: &Path,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<Session> {
    let settings = crate::SearchSettings {
        method: crate::ScanMethod::Carve,
        carve_scope: crate::CarveScope::All,
        ..Default::default()
    };
    scan_range(path, options, &settings, cancel, progress)
}

pub(crate) fn scan_range(
    path: &Path,
    options: ScanOptions,
    settings: &crate::SearchSettings,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Session> {
    let start = settings.range_start;
    let end = settings.range_end;
    let extensions = &settings.extensions;
    let mut image = Image::open(path)?;
    let fingerprint = image.fingerprint()?;
    let boundary = end.unwrap_or(image.length);
    if start > boundary || boundary > image.length {
        return Err(Error::Request("scan range outside image".into()));
    }
    let mut session = Session {
        source_info: None,
        raid: None,
        schema_version: 6, filesystem: FileSystem::Raw, id: Uuid::new_v4().to_string(),
        source: image.path.clone(), fingerprint, candidates: Vec::new(), scanned_records: 0,
        total_records: boundary-start, cancelled: false, truncated: false, invalid_records: 0,
        search: Some(settings.clone()),
        region: None, volumes: Vec::new(),
        warnings: vec![PROVENANCE.into(), "PNG/APNG/JPEG/HEIC/AVIF/PSD/PSB/OpenEXR/TIFF/Netpbm/PDF/RTF/HTML/XML/JSON/RAR/ZIP/Office/OpenDocument/EPUB/MP4/MOV/MP3/FLAC/AIFF/AIFF-C/TTF/OTF/WOFF/GIF/BMP/WebP/ICO/CUR/WAV/SQLite/AVI/MKV/WebM/Ogg/gzip/bzip2/XZ/lzip/Zstandard/LZ4 contiguous carving; <=64 MiB encoded / pixel reservation, <=128 MiB PNG scanlines or JPEG coefficients, <=65536 markers/chunks; JPEG <=256 scans. Shared 4 GiB validation budget. Progress uses bytes. Metadata contents, deletion status and original bytes are not established by structure or pixel decoding.".into()],
    };
    let mut scope = scope::Scope::new(&mut image, settings.carve_scope)?;
    let mut json_end = 0;
    let mut aac_end = 0;
    let mut internal_headers = std::collections::BTreeSet::new();
    let mut attempts = 0u64;
    let mut offset = start;
    let mut budget = VALIDATION_BUDGET;
    let mut missing = false;
    'scan: while offset < boundary {
        if cancel.load(Ordering::Relaxed) {
            session.cancelled = true;
            break;
        }
        let count = WINDOW.min(boundary - offset);
        let (buffer, gaps) = image.read_available(
            offset,
            (count + SIGNATURE_LOOKAHEAD).min(boundary - offset) as usize,
        )?;
        missing |= !gaps.is_empty();
        for index in signature_offsets(&buffer, count as usize, settings.signature_rule.as_ref()) {
            let rest = &buffer[index..];
            let Some(format) = detect(
                rest,
                extensions,
                settings.signature_rule.as_ref(),
                settings.tar_blocking_factor,
            ) else {
                continue;
            };
            let at = offset + index as u64;
            if (format == Format::Json && at < json_end) || (format == Format::Aac && at < aac_end)
            {
                continue;
            }
            let allocation = match scope.status(at, 1, cancel) {
                Ok(status) => status,
                Err(Error::Cancelled) => {
                    session.cancelled = true;
                    session.scanned_records = at - start;
                    break 'scan;
                }
                Err(error) => return Err(error),
            };
            if allocation == AllocationStatus::Conflict {
                continue;
            }
            if matches!(
                format,
                Format::Zip
                    | Format::Html
                    | Format::Xml
                    | Format::Svg
                    | Format::Mp3
                    | Format::Tar(_)
                    | Format::J2k
            ) && internal_headers.remove(&(offset + index as u64))
            {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                session.cancelled = true;
                break 'scan;
            }
            session.scanned_records = offset + index as u64 - start;
            if session.candidates.len() >= options.max_candidates || attempts >= options.max_records
            {
                session.truncated = true;
                break 'scan;
            }
            attempts += 1;
            let at = offset + index as u64;
            let parsed = format.validate(&mut image, at, cancel, &mut budget, boundary);
            let mut candidate = Candidate {
                fragment_offset: None,
                id: format.id(at),
                record: at,
                sequence: 0,
                stream_name: None,
                name: format.name(at),
                original_path: format.name(at),
                path_verified: false,
                alternate_names: Vec::new(),
                size: None,
                supported: false,
                warnings: vec![PROVENANCE.into()],
                record_sha256: String::new(),
                extent_evidence: ExtentEvidence::Unknown,
                allocation_status: AllocationStatus::Unknown,
                created: None,
                modified: None,
            };
            match parsed {
                Ok((validated_format, bytes, records)) => {
                    let validated_format = validated_format.for_extensions(extensions);
                    candidate.allocation_status = match scope.status(at, bytes.len() as u64, cancel)
                    {
                        Ok(status) => status,
                        Err(Error::Cancelled) => {
                            session.cancelled = true;
                            break 'scan;
                        }
                        Err(error) => return Err(error),
                    };
                    if candidate.allocation_status == AllocationStatus::Conflict {
                        continue;
                    }
                    if validated_format == Format::Aac {
                        aac_end = at + bytes.len() as u64;
                    }
                    if validated_format == Format::Json {
                        json_end = at + bytes.len() as u64;
                    }
                    candidate.id = validated_format.id(at);
                    candidate.name = validated_format.name(at);
                    candidate.original_path = candidate.name.clone();
                    candidate.warnings.extend(
                        format_warnings(validated_format)
                            .iter()
                            .map(|s| s.to_string()),
                    );
                    for record in records.into_iter().filter(|r| *r > 0) {
                        if internal_headers.len() >= 1_048_576 {
                            session.truncated = true;
                            session.warnings.push(
                                "Internal signature index limit; remaining source was not scanned"
                                    .into(),
                            );
                            break;
                        }
                        internal_headers.insert(at + record as u64);
                    }
                    candidate.size = Some(bytes.len() as u64);
                    candidate.supported = true;
                    candidate.record_sha256 = format!("{:x}", Sha256::digest(&bytes));
                    candidate.extent_evidence = validated_format.evidence();
                }
                Err(Error::Cancelled) => {
                    session.cancelled = true;
                    break 'scan;
                }
                Err(Error::Io(e)) => return Err(e.into()),
                Err(e) => {
                    session.invalid_records += 1;
                    candidate.warnings.push(format!("{}: {e}", e.code()));
                    if matches!(&e, Error::Limit(reason) if reason.contains("budget")) {
                        session.truncated = true;
                        session.warnings.push(e.to_string());
                    }
                }
            }
            if !matches!(format, Format::Png | Format::Jpeg) {
                session.schema_version = 7;
            }
            session.candidates.push(candidate);
            if session.truncated {
                break 'scan;
            }
        }
        offset += count;
        session.scanned_records = offset - start;
        scope.append_warnings(&mut session);
        progress(Progress {
            scanned_records: offset - start,
            total_records: boundary - start,
            candidates: session.candidates.len(),
            unit: ProgressUnit::Bytes,
        });
    }
    if session.truncated {
        session
            .warnings
            .push("Scan stopped at a resource limit; remaining bytes were not searched.".into());
    }
    scope.append_warnings(&mut session);
    if missing {
        session.truncated = true;
        session.warnings.push("MISSING_RANGES: acquired image has unread/failed bytes; signatures there were not searched. Parsers reject payloads crossing a missing range.".into());
    }
    session.bound_candidates()?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    progress(Progress {
        scanned_records: session.scanned_records,
        total_records: boundary - start,
        candidates: session.candidates.len(),
        unit: ProgressUnit::Bytes,
    });
    image.annotate_coverage(&mut session)?;
    Ok(session)
}

pub(crate) fn recovery_data(
    image: &mut Image,
    candidate: &Candidate,
    cancel: &AtomicBool,
    rule: Option<&SignatureRule>,
) -> Result<RecoveryData> {
    let blocking = candidate
        .id
        .rsplit_once("-b")
        .and_then(|(_, n)| n.parse::<u16>().ok())
        .filter(|n| (1..=1024).contains(n))
        .unwrap_or(1);
    let format = [
        Format::Tar(blocking),
        Format::Asf,
        Format::Wma,
        Format::Wmv,
        Format::Rar,
        Format::SevenZ,
        Format::Cab,
        Format::Png,
        Format::Apng,
        Format::Jpeg,
        Format::Pdf,
        Format::Tiff,
        Format::Jp2,
        Format::J2k,
        Format::Exr,
        Format::Psd,
        Format::Psb,
        Format::Aiff,
        Format::Aifc,
        Format::Flac,
        Format::Ttf,
        Format::Otf,
        Format::Woff,
        Format::Mp3,
        Format::Aac,
        Format::Pbm,
        Format::Pgm,
        Format::Ppm,
        Format::Pam,
        Format::Html,
        Format::Xml,
        Format::Svg,
        Format::Svgz,
        Format::Json,
        Format::Rtf,
        Format::Zip,
        Format::Docx,
        Format::Xlsx,
        Format::Pptx,
        Format::Heic,
        Format::Avif,
        Format::Mp4,
        Format::Mov,
        Format::Wav,
        Format::Webp,
        Format::Bmp,
        Format::Gif,
        Format::Avi,
        Format::Mkv,
        Format::Webm,
        Format::Ogg,
        Format::Gzip,
        Format::Ico,
        Format::Cur,
        Format::Zstd,
        Format::Lz4,
        Format::Bzip,
        Format::Xz,
        Format::Lzip,
        Format::Sqlite,
    ]
    .into_iter()
    .chain(zip::package::formats())
    .filter(|_| rule.is_none())
    .chain(rule.map(Format::Rule))
    .find(|f| candidate.id == f.id(candidate.record))
    .ok_or(Error::SourceChanged)?;
    if candidate.id != format.id(candidate.record)
        || candidate.sequence != 0
        || candidate.stream_name.is_some()
    {
        return Err(Error::SourceChanged);
    }
    let mut budget = VALIDATION_BUDGET;
    let boundary = if format == Format::Aac {
        let length = candidate.size.ok_or(Error::SourceChanged)?;
        candidate
            .record
            .checked_add(length)
            .filter(|end| *end <= image.length)
            .ok_or(Error::SourceChanged)?
    } else {
        image.length
    };
    let (actual, bytes, _) =
        format.validate(image, candidate.record, cancel, &mut budget, boundary)?;
    // Sessions made before package classification recorded ODF/EPUB as generic ZIP.
    if actual != format
        && !matches!(
            (format, actual),
            (Format::Zip, Format::Package(_))
                | (Format::Asf, Format::Wma | Format::Wmv)
                | (Format::Xml, Format::Svg)
                | (Format::Gzip, Format::Svgz)
        )
    {
        return Err(Error::SourceChanged);
    }
    if candidate.record_sha256 != format!("{:x}", Sha256::digest(&bytes)) {
        return Err(Error::SourceChanged);
    }
    Ok(RecoveryData {
        data: Data::Resident(bytes),
        name: format.name(candidate.record),
        warnings: std::iter::once(PROVENANCE)
            .chain(format_warnings(format).iter().copied())
            .map(str::to_owned)
            .collect(),
        extent_evidence: format.evidence(),
        allocation_status: AllocationStatus::Unknown,
    })
}

pub(crate) fn revalidate_candidate(
    image: &mut Image,
    candidate: &Candidate,
    cancel: &AtomicBool,
    rule: Option<&SignatureRule>,
) -> Result<Candidate> {
    let data = recovery_data(image, candidate, cancel, rule)?;
    Ok(Candidate {
        id: candidate.id.clone(),
        record: candidate.record,
        sequence: 0,
        fragment_offset: None,
        stream_name: None,
        original_path: data.name.clone(),
        name: data.name,
        path_verified: false,
        alternate_names: Vec::new(),
        size: Some(data.data.size()),
        supported: true,
        warnings: data.warnings,
        record_sha256: candidate.record_sha256.clone(),
        extent_evidence: data.extent_evidence,
        allocation_status: data.allocation_status,
        created: None,
        modified: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_mutations_remain_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mutated.raw");
        let cancel = AtomicBool::new(false);
        let samples: &[(Format, &[u8])] = &[
            (
                Format::Pdf,
                include_bytes!("../../../tests/corpus/files/sample.pdf"),
            ),
            (
                Format::Zip,
                include_bytes!("../../../tests/corpus/files/sample.zip"),
            ),
            (
                Format::Docx,
                include_bytes!("../../../tests/corpus/files/sample.docx"),
            ),
            (
                Format::Mp4,
                include_bytes!("../../../tests/corpus/files/sample.mp4"),
            ),
        ];
        let mut random = 0xb10c_5eed_u64;
        for (format, original) in samples {
            for case in 0..256 {
                let mut bytes = original.to_vec();
                for _ in 0..4 {
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let at = random as usize % bytes.len();
                    bytes[at] = (random >> 32) as u8;
                }
                if case % 3 == 0 {
                    bytes.truncate(random as usize % bytes.len());
                }
                std::fs::write(&path, &bytes).unwrap();
                let mut image = Image::open(&path).unwrap();
                // Result may be unsupported or even another valid structure; panic is always wrong.
                let _ = format.validate(
                    &mut image,
                    0,
                    &cancel,
                    &mut (4 * 1024 * 1024),
                    bytes.len() as u64,
                );
            }
        }
    }
    #[test]
    fn signature_boundaries_limits_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.raw");
        let mut input = vec![0; WINDOW as usize + 32];
        input[WINDOW as usize - 3..WINDOW as usize + 5].copy_from_slice(SIGNATURE);
        std::fs::write(&path, input).unwrap();
        let stop = AtomicBool::new(false);
        let s = scan_carving(&path, ScanOptions::default(), &stop, |_| {}).unwrap();
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0].record, WINDOW - 3);
        assert!(!s.candidates[0].supported);
        let s = scan_carving(
            &path,
            ScanOptions {
                fragments: false,
                estimate_fat_contiguous: false,
                fat_short_name_encoding: crate::FatShortNameEncoding::Ascii,
                exfat_allow_damaged_upcase: false,
                max_records: 0,
                max_candidates: 1,
            },
            &stop,
            |_| {},
        )
        .unwrap();
        assert!(s.truncated && s.candidates.is_empty());
        stop.store(true, Ordering::Relaxed);
        let s = scan_carving(&path, ScanOptions::default(), &stop, |_| {}).unwrap();
        assert!(s.cancelled && s.scanned_records == 0);
        assert!(charge(&mut 7, 8).is_err());
    }
}
