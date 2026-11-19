//! Read EWF segments with libewf tools and independently compare the exported logical bytes.
use super::ImportedFile;
use super::{executable_in_path, publish_output, read_tool};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

fn diagnostic(
    name: &str,
    options: &[&str],
    paths: &[OsString],
    cancel: &AtomicBool,
) -> Result<String> {
    let args = options
        .iter()
        .map(OsString::from)
        .chain(paths.iter().cloned())
        .collect::<Vec<_>>();
    read_tool(name, &args, cancel, |reader| {
        let mut bytes = Vec::new();
        reader.take(262145).read_to_end(&mut bytes)?;
        if bytes.len() > 262144 {
            return Err(Error::Limit("EWF diagnostic exceeds 256 KiB".into()));
        }
        String::from_utf8(bytes)
            .map_err(|_| Error::Invalid("unrecognized libewf diagnostic encoding".into()))
    })
}

fn expected_hash(paths: &[OsString], cancel: &AtomicBool) -> Result<String> {
    let report = diagnostic("ewfinfo", &["-e"], paths, cancel)?;
    // An empty acquisition-error report contains only the tool version. A RAW
    // image cannot retain the difference between a missing sector and a verified zero.
    if report
        .lines()
        .map(str::trim)
        .any(|line| !line.is_empty() && !line.starts_with("ewfinfo "))
    {
        return Err(Error::Unsupported("EWF acquisition errors or unrecognized error report; preserve the original segments and their read-error metadata".into()));
    }
    let verification = diagnostic("ewfverify", &["-q", "-d", "sha256"], paths, cancel)?;
    verification
        .lines()
        .find_map(|line| line.strip_prefix("SHA256 hash calculated over data:"))
        .map(str::trim)
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| Error::Invalid("libewf did not report a verified logical SHA-256".into()))
}

fn export(reader: &mut impl Read, output: &mut File, expected: &str) -> Result<(u64, Sha256)> {
    // Some libewf versions append their success message to stdout even in raw
    // mode. Hold only this suffix and let the independent digest identify the exact end.
    const LF: &[u8] = b"ewfexport: SUCCESS\n";
    const CRLF: &[u8] = b"ewfexport: SUCCESS\r\n";
    let mut buf = vec![0; 65536];
    let mut held = 0;
    let mut bytes = 0u64;
    let mut digest = Sha256::new();
    loop {
        let read = reader.read(&mut buf[held..])?;
        if read == 0 {
            break;
        }
        let total = held + read;
        let copy = total.saturating_sub(CRLF.len());
        output.write_all(&buf[..copy])?;
        digest.update(&buf[..copy]);
        bytes += copy as u64;
        buf.copy_within(copy..total, 0);
        held = total - copy;
    }
    let final_bytes = &buf[..held];
    for suffix in [b"".as_slice(), LF, CRLF] {
        if let Some(last) = final_bytes.strip_suffix(suffix) {
            let mut candidate = digest.clone();
            candidate.update(last);
            if format!("{:x}", candidate.clone().finalize()) == expected {
                output.write_all(last)?;
                return Ok((bytes + last.len() as u64, candidate));
            }
        }
    }
    Err(Error::Invalid(
        "exported EWF bytes differ from the independently verified SHA-256".into(),
    ))
}

pub(super) fn import(
    paths: &[OsString],
    kind: Option<&str>,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if kind.is_some_and(|v| !matches!(v, "e01" | "ewf" | "ewf-verified")) {
        return Ok(None);
    }
    if kind.is_none() {
        let mut magic = Vec::with_capacity(8);
        File::open(&paths[0])?.take(8).read_to_end(&mut magic)?;
        if !matches!(
            magic.as_slice(),
            b"EVF\x09\x0d\x0a\xff\0" | b"EVF2\r\n\x81\0"
        ) {
            return Ok(None);
        }
    }
    if ["ewfinfo", "ewfverify", "ewfexport"]
        .iter()
        .any(|n| executable_in_path(n).is_none())
    {
        return if matches!(kind, Some("ewf-verified" | "e01")) {
            Err(Error::Unsupported(
                "install ewfinfo, ewfverify and ewfexport from libewf tools".into(),
            ))
        } else {
            Ok(None)
        };
    }
    let mut args = vec![
        "-q".into(),
        "-u".into(),
        "-f".into(),
        "raw".into(),
        "-t".into(),
        "-".into(),
    ];
    args.extend(paths.iter().cloned());
    let mut result = publish_output(output, cancel, |file| {
        let expected = expected_hash(paths, cancel)?;
        read_tool("ewfexport", &args, cancel, |reader| {
            export(reader, file, &expected)
        })
    })?;
    result.ewf = Some(super::EwfVerification {
        reader: "libewf",
        logical_sha256_verified: true,
    });
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn raw_bytes_can_end_in_a_success_message() {
        for length in [0, 13, 65536, 65551] {
            let mut payload = vec![17; length];
            payload.extend_from_slice(b"ewfexport: SUCCESS\n");
            let expected = format!("{:x}", Sha256::digest(&payload));
            for suffix in [
                b"".as_slice(),
                b"ewfexport: SUCCESS\n",
                b"ewfexport: SUCCESS\r\n",
            ] {
                let wire = [payload.as_slice(), suffix].concat();
                let mut file = tempfile::tempfile().unwrap();
                let (written, _) = export(&mut wire.as_slice(), &mut file, &expected).unwrap();
                file.seek(SeekFrom::Start(0)).unwrap();
                let mut actual = Vec::new();
                file.read_to_end(&mut actual).unwrap();
                assert_eq!(actual, payload);
                assert_eq!(written, payload.len() as u64);
            }
        }
    }
}
