//! Read the storage and snapshot relationships from a single-storage DiskDescriptor.xml.
use super::invalid;
use crate::{Error, Result};
use roxmltree::Node;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub(super) const MAX_DESCRIPTOR: u64 = 1024 * 1024;
pub(super) const MAX_LAYERS: usize = 8;

pub(super) struct Member {
    pub name: String,
    pub plain: bool,
}

pub(super) struct Descriptor {
    pub sectors: u64,
    pub cluster_sectors: u64,
    pub layers: Vec<Member>,
}

fn children(node: Node<'_, '_>, allowed: &[&str]) -> Result<()> {
    if node.attributes().len() != 0
        || node.children().any(|child| {
            child.is_element()
                && (child.tag_name().namespace().is_some()
                    || !allowed.contains(&child.tag_name().name()))
                || child.is_text() && !child.text().unwrap_or_default().trim().is_empty()
        })
    {
        return Err(invalid(
            "unsupported Parallels descriptor elements or attributes",
        ));
    }
    Ok(())
}

fn one<'a, 'input>(parent: Node<'a, 'input>, name: &str) -> Result<Node<'a, 'input>> {
    let mut matching = parent.children().filter(|child| child.has_tag_name(name));
    let item = matching
        .next()
        .ok_or_else(|| invalid("missing Parallels descriptor field"))?;
    if matching.next().is_some() {
        return Err(invalid("duplicate Parallels descriptor field"));
    }
    Ok(item)
}

fn value<'a>(parent: Node<'a, '_>, name: &str) -> Result<&'a str> {
    let node = one(parent, name)?;
    if node.attributes().len() != 0
        || node.children().count() != 1
        || !node.first_child().is_some_and(|c| c.is_text())
    {
        return Err(invalid("expected a Parallels descriptor value"));
    }
    node.text()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("empty Parallels descriptor value"))
}

fn number(parent: Node<'_, '_>, name: &str) -> Result<u64> {
    value(parent, name)?
        .parse()
        .map_err(|_| invalid("invalid Parallels descriptor number"))
}

fn guid(parent: Node<'_, '_>, name: &str) -> Result<Uuid> {
    Uuid::parse_str(value(parent, name)?).map_err(|_| invalid("invalid Parallels descriptor GUID"))
}

pub(super) fn parse(xml: &[u8]) -> Result<Descriptor> {
    let xml =
        std::str::from_utf8(xml).map_err(|_| invalid("Parallels descriptor must be UTF-8"))?;
    let doc = roxmltree::Document::parse_with_options(
        xml.trim_start_matches('\u{feff}'),
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .map_err(|_| invalid("invalid Parallels descriptor XML"))?;
    let root = doc.root_element();
    if root.tag_name().namespace().is_some() || !root.has_tag_name("Parallels_disk_image") {
        return Err(invalid("expected a Parallels_disk_image descriptor"));
    }
    children(root, &["Disk_Parameters", "StorageData", "Snapshots"])?;
    let params = one(root, "Disk_Parameters")?;
    children(
        params,
        &["Disk_size", "Cylinders", "Heads", "Sectors", "Padding"],
    )?;
    let sectors = number(params, "Disk_size")?;
    if params.children().any(|c| c.has_tag_name("Padding")) && number(params, "Padding")? != 0 {
        return Err(invalid("unsupported Parallels disk padding"));
    }
    let data = one(root, "StorageData")?;
    children(data, &["Storage"])?;
    let storage = one(data, "Storage")?;
    children(storage, &["Start", "End", "Blocksize", "Image"])?;
    let cluster_sectors = number(storage, "Blocksize")?;
    if number(storage, "Start")? != 0
        || number(storage, "End")? != sectors
        || !super::geometry(sectors, cluster_sectors)
    {
        return Err(invalid("unsupported Parallels storage geometry"));
    }
    let mut images = HashMap::new();
    let mut names = HashSet::new();
    for image in storage.children().filter(|n| n.has_tag_name("Image")) {
        children(image, &["GUID", "Type", "File"])?;
        let id = guid(image, "GUID")?;
        let plain = match value(image, "Type")? {
            "Compressed" => false,
            "Plain" => true,
            _ => return Err(invalid("unknown Parallels image type")),
        };
        let name = value(image, "File")?;
        if name.len() > 255
            || name.contains(['/', '\\', ':'])
            || matches!(name, "." | "..")
            || name.chars().any(char::is_control)
            || id.is_nil()
            || !names.insert(name.to_lowercase())
            || images
                .insert(
                    id,
                    Member {
                        name: name.to_owned(),
                        plain,
                    },
                )
                .is_some()
        {
            return Err(invalid("invalid or repeated Parallels image name or GUID"));
        }
    }
    if images.is_empty() || images.len() > MAX_LAYERS {
        return Err(Error::Unsupported(
            "select a Parallels descriptor with one to eight image layers".into(),
        ));
    }
    let snapshots = one(root, "Snapshots")?;
    children(snapshots, &["TopGUID", "Shot"])?;
    let mut id = guid(snapshots, "TopGUID")?;
    let mut parents = HashMap::new();
    for shot in snapshots.children().filter(|n| n.has_tag_name("Shot")) {
        children(shot, &["GUID", "ParentGUID"])?;
        let child = guid(shot, "GUID")?;
        if !images.contains_key(&child)
            || parents.insert(child, guid(shot, "ParentGUID")?).is_some()
        {
            return Err(invalid("unmatched or repeated Parallels snapshot"));
        }
    }
    let mut layers = Vec::new();
    while !id.is_nil() {
        let member = images
            .remove(&id)
            .ok_or_else(|| invalid("missing or cyclic Parallels snapshot chain"))?;
        id = parents
            .remove(&id)
            .ok_or_else(|| invalid("missing Parallels parent link"))?;
        layers.push(member);
    }
    if layers.is_empty() || !images.is_empty() || !parents.is_empty() {
        return Err(invalid(
            "Parallels snapshot graph must be one complete chain",
        ));
    }
    // ploop 1.15 emits Type=Plain for snapshots of a flat base too. Those
    // upper files have real HDS headers and allocation tables; only the base
    // may actually be read as raw bytes.
    let (base, overlays) = layers
        .split_last_mut()
        .ok_or_else(|| invalid("empty Parallels chain"))?;
    for layer in overlays {
        if layer.plain && !base.plain {
            return Err(invalid("Parallels plain overlay without a flat base"));
        }
        layer.plain = false;
    }
    Ok(Descriptor {
        sectors,
        cluster_sectors,
        layers,
    })
}
