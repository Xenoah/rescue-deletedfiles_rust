//! Bound a self-contained glTF 2.0 binary container and its embedded data ranges.
use super::container::Input;
use crate::{Error, Result};
use gltf_json::{
    Root,
    accessor::Type,
    validation::{Checked, Validate},
};

const MAX_JSON: usize = 2 * 1024 * 1024;

fn invalid() -> Error {
    Error::Invalid("invalid or unsupported self-contained glTF 2.0 binary structure".into())
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn check_scene_tree(root: &Root) -> Result<()> {
    let mut has_parent = vec![false; root.nodes.len()];
    for node in &root.nodes {
        for child in node.children.iter().flatten() {
            let slot = has_parent.get_mut(child.value()).ok_or_else(invalid)?;
            if std::mem::replace(slot, true) {
                return Err(invalid());
            }
        }
    }
    let mut pending: Vec<_> = (0..root.nodes.len()).filter(|&i| !has_parent[i]).collect();
    let mut visited = 0;
    while let Some(i) = pending.pop() {
        visited += 1;
        pending.extend(
            root.nodes[i]
                .children
                .iter()
                .flatten()
                .map(|child| child.value()),
        );
    }
    if visited != root.nodes.len()
        || root
            .scenes
            .iter()
            .flat_map(|scene| &scene.nodes)
            .any(|n| has_parent.get(n.value()) != Some(&false))
    {
        return Err(invalid());
    }
    Ok(())
}

fn check_data(root: &Root, bin: &[u8]) -> Result<()> {
    let bytes = match root.buffers.as_slice() {
        [] if bin.is_empty() => 0,
        [b] if b.uri.is_none() && b.byte_length.0 != 0 => b.byte_length.0,
        _ => return Err(invalid()),
    };
    if bytes > bin.len() as u64
        || bin.len() as u64 - bytes > 3
        || bin[bytes as usize..].iter().any(|&x| x != 0)
    {
        return Err(invalid());
    }
    for view in &root.buffer_views {
        let start = view.byte_offset.map_or(0, |n| n.0);
        if view.buffer.value() != 0
            || view.byte_length.0 == 0
            || start
                .checked_add(view.byte_length.0)
                .is_none_or(|end| end > bytes)
        {
            return Err(invalid());
        }
    }
    for accessor in &root.accessors {
        // Sparse accessor overrides require separate index and value validation.
        if accessor.sparse.is_some() || accessor.count.0 == 0 {
            return Err(invalid());
        }
        let view = root
            .buffer_views
            .get(accessor.buffer_view.ok_or_else(invalid)?.value())
            .ok_or_else(invalid)?;
        let (Checked::Valid(component), Checked::Valid(kind)) =
            (accessor.component_type, accessor.type_)
        else {
            return Err(invalid());
        };
        let width = component.0.size() as u64;
        let (element, last, alignment) = match kind {
            Type::Mat2 | Type::Mat3 | Type::Mat4 => {
                let columns = match kind {
                    Type::Mat2 => 2,
                    Type::Mat3 => 3,
                    _ => 4,
                };
                let column = columns * width;
                let padded = column.next_multiple_of(4);
                (columns * padded, (columns - 1) * padded + column, 4)
            }
            _ => {
                let size = width * kind.multiplicity() as u64;
                (size, size, width)
            }
        };
        let start = accessor.byte_offset.map_or(0, |n| n.0);
        let stride = view.byte_stride.map_or(element, |s| s.0 as u64);
        let end = (accessor.count.0 - 1)
            .checked_mul(stride)
            .and_then(|n| n.checked_add(start))
            .and_then(|n| n.checked_add(last));
        if stride < element
            || !stride.is_multiple_of(alignment)
            || !start.is_multiple_of(width)
            || !(view.byte_offset.map_or(0, |n| n.0)).is_multiple_of(width)
            || !view
                .byte_offset
                .map_or(0, |n| n.0)
                .checked_add(start)
                .is_some_and(|v| v.is_multiple_of(alignment))
            || end.is_none_or(|end| end > view.byte_length.0)
        {
            return Err(invalid());
        }
    }
    for image in &root.images {
        let view = root
            .buffer_views
            .get(image.buffer_view.ok_or_else(invalid)?.value())
            .ok_or_else(invalid)?;
        let start = view.byte_offset.map_or(0, |v| v.0) as usize;
        let data = &bin[start..start + view.byte_length.0 as usize];
        let agrees = match image.mime_type.as_ref().map(|m| m.0.as_str()) {
            Some("image/png") => data.starts_with(b"\x89PNG\r\n\x1a\n"),
            Some("image/jpeg") => data.starts_with(b"\xff\xd8\xff"),
            _ => false,
        };
        if image.uri.is_some() || !agrees {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(20)?;
    let head = &input.bytes[..20];
    let total = word(head, 8) as usize;
    let json_len = word(head, 12) as usize;
    if &head[..4] != b"glTF"
        || word(head, 4) != 2
        || &head[16..20] != b"JSON"
        || !total.is_multiple_of(4)
        || !json_len.is_multiple_of(4)
        || !(4..=MAX_JSON).contains(&json_len)
        || 20 + json_len > total
        || total > input.limit()
    {
        return Err(invalid());
    }
    let split = 20 + json_len;
    input.ensure(split)?;
    let root = Root::from_slice(&input.bytes[20..split]).map_err(|_| invalid())?;
    if root.asset.version != "2.0"
        || root
            .asset
            .min_version
            .as_deref()
            .is_some_and(|v| v != "2.0")
        || !root.extensions_required.is_empty()
    {
        return Err(invalid());
    }
    // gltf-json's primitive validation needs each referenced POSITION accessor to exist.
    if root
        .meshes
        .iter()
        .flat_map(|m| &m.primitives)
        .flat_map(|p| p.attributes.values())
        .any(|index| index.value() >= root.accessors.len())
    {
        return Err(invalid());
    }
    // Bound and type-check data that the schema validator inspects for each primitive.
    for accessor in &root.accessors {
        let Checked::Valid(kind) = accessor.type_ else {
            return Err(invalid());
        };
        for bounds in [&accessor.min, &accessor.max].into_iter().flatten() {
            if !bounds.as_array().is_some_and(|values| {
                values.len() == kind.multiplicity()
                    && values.iter().all(serde_json::Value::is_number)
            }) {
                return Err(invalid());
            }
        }
    }
    let mut valid = true;
    root.validate(&root, gltf_json::Path::new, &mut |_, _| valid = false);
    if !valid {
        return Err(invalid());
    }
    check_scene_tree(&root)?;
    let bin = if split == total {
        &[][..]
    } else {
        if total - split < 8 {
            return Err(invalid());
        }
        input.ensure(split + 8)?;
        let header = &input.bytes[split..split + 8];
        let length = word(header, 0) as usize;
        if &header[4..] != b"BIN\0" || length == 0 || length != total - split - 8 {
            return Err(invalid());
        }
        input.ensure(total)?;
        &input.bytes[split + 8..total]
    };
    check_data(&root, bin)?;
    Ok(total)
}
