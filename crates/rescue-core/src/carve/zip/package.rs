//! Identify ODF and EPUB only after verifying the complete ZIP and its CRCs.
use super::{Entry, Format, Input, Result, invalid, u16le};
use std::io::Read;

const MIME_PREFIX: &str = "application/vnd.oasis.opendocument.";
const MANIFEST: &str = "urn:oasis:names:tc:opendocument:xmlns:manifest:1.0";
const CONTAINER: &str = "urn:oasis:names:tc:opendocument:xmlns:container";
const OPF: &str = "http://www.idpf.org/2007/opf";
const XML_LIMIT: usize = 1024 * 1024;

// Extension and the registered OASIS media subtype (EPUB uses a separate media type).
const ODF: &[(&str, &str)] = &[
    ("odt", "text"),
    ("ods", "spreadsheet"),
    ("odp", "presentation"),
    ("odg", "graphics"),
    ("odf", "formula"),
    ("odc", "chart"),
    ("odb", "database"),
    ("odm", "text-master"),
    ("oth", "text-web"),
    ("ott", "text-template"),
    ("ots", "spreadsheet-template"),
    ("otp", "presentation-template"),
    ("otg", "graphics-template"),
    ("otc", "chart-template"),
    ("otf", "formula-template"),
    ("odi", "image"),
    ("oti", "image-template"),
];

pub(in crate::carve) fn formats<'a>() -> impl Iterator<Item = Format<'a>> {
    ODF.iter()
        .map(|&(ext, _)| Format::Package(ext))
        .chain([Format::Package("epub")])
}

fn member<'a>(entries: &'a [Entry], name: &[u8]) -> Option<&'a Entry> {
    entries.iter().find(|e| e.name == name)
}

fn payload<'a>(archive: &'a [u8], entry: &Entry) -> &'a [u8] {
    // validate_entries has already checked these bounds and local name/extra lengths.
    let p = entry.local;
    let at = p + 30 + usize::from(u16le(archive, p + 26)) + usize::from(u16le(archive, p + 28));
    &archive[at..at + entry.compressed]
}

fn xml_member(input: &mut Input<'_>, entry: Option<&Entry>) -> Result<Option<String>> {
    let Some(entry) = entry.filter(|e| e.decoded <= XML_LIMIT) else {
        return Ok(None);
    };
    input.work(entry.compressed + entry.decoded)?;
    let encoded = payload(&input.bytes, entry);
    let bytes = if entry.method == 0 {
        encoded.to_vec()
    } else {
        let mut xml = Vec::with_capacity(entry.decoded);
        flate2::read::DeflateDecoder::new(encoded)
            .take(XML_LIMIT as u64 + 1)
            .read_to_end(&mut xml)?;
        if xml.len() != entry.decoded {
            return Err(invalid("package manifest length"));
        }
        xml
    };
    Ok(String::from_utf8(bytes).ok())
}

fn xml(text: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 32768,
            allow_dtd: false,
            entity_resolver: None,
        },
    )
    .ok()
}

fn odf_manifest(text: &str, mime: &str) -> bool {
    let Some(doc) = xml(text) else { return false };
    let root = doc.root_element();
    if !root.has_tag_name((MANIFEST, "manifest")) {
        return false;
    }
    let mut document = 0;
    let mut content = 0;
    for n in root
        .children()
        .filter(|n| n.has_tag_name((MANIFEST, "file-entry")))
    {
        match n.attribute((MANIFEST, "full-path")) {
            Some("/") if n.attribute((MANIFEST, "media-type")) == Some(mime) => document += 1,
            Some("content.xml")
                if matches!(
                    n.attribute((MANIFEST, "media-type")),
                    Some("text/xml" | "application/xml")
                ) =>
            {
                content += 1
            }
            Some("/" | "content.xml") => return false,
            _ => {}
        }
    }
    document == 1 && content == 1
}

fn primary_opf(text: &str) -> Option<String> {
    let tree = xml(text)?;
    let root = tree.root_element();
    if !root.has_tag_name((CONTAINER, "container")) {
        return None;
    }
    let files = root
        .children()
        .find(|n| n.has_tag_name((CONTAINER, "rootfiles")))?;
    let first = files
        .children()
        .find(|n| n.has_tag_name((CONTAINER, "rootfile")))?;
    let path = first.attribute("full-path")?;
    if first.attribute("media-type") != Some("application/oebps-package+xml")
        || path.contains(['\\', '\0', '?', '#', ':'])
        || path.split('/').any(|s| matches!(s, "" | "." | ".."))
    {
        return None;
    }
    Some(path.to_owned())
}

fn opf_package(text: &str) -> bool {
    let Some(doc) = xml(text) else { return false };
    let root = doc.root_element();
    root.has_tag_name((OPF,"package")) && root.attribute("version").is_some_and(|v|
        matches!(v.split_once('.'),Some(("2"|"3",minor)) if !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit())))
        && ["metadata","manifest","spine"].into_iter().all(|name| root.children().filter(|n|n.has_tag_name((OPF,name))).count()==1)
}

pub(super) fn identify(
    input: &mut Input<'_>,
    entries: &[Entry],
) -> Result<Option<Format<'static>>> {
    let Some(first) = member(entries, b"mimetype") else {
        return Ok(None);
    };
    if first.local != 0
        || first.method != 0
        || first.decoded > 128
        || u16le(&input.bytes, 28) != 0
        || first.flags & 8 != 0
    {
        return Ok(None);
    }
    let raw = payload(&input.bytes, first);
    if raw == b"application/epub+zip" {
        let Some(container) = xml_member(input, member(entries, b"META-INF/container.xml"))? else {
            return Ok(None);
        };
        let Some(path) = primary_opf(&container) else {
            return Ok(None);
        };
        let Some(opf) = xml_member(input, member(entries, path.as_bytes()))? else {
            return Ok(None);
        };
        return Ok(opf_package(&opf).then_some(Format::Package("epub")));
    }
    let Some((extension, mime)) = std::str::from_utf8(raw).ok().and_then(|mime| {
        let subtype = mime.strip_prefix(MIME_PREFIX)?;
        let &(ext, _) = ODF.iter().find(|(_, v)| *v == subtype)?;
        Some((ext, mime.to_owned()))
    }) else {
        return Ok(None);
    };
    if member(entries, b"content.xml").is_none() {
        return Ok(None);
    }
    let manifest = xml_member(input, member(entries, b"META-INF/manifest.xml"))?;
    Ok(manifest
        .is_some_and(|text| odf_manifest(&text, &mime))
        .then_some(Format::Package(extension)))
}
