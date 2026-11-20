use clap::Args;
use rescue_core::{
    Result,
    disk_image::{self, FilesystemRequest, ImageInput},
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
#[derive(Args)]
pub struct ImageArgs {
    /// Read-only disk, volume, or first image segment.
    #[arg(long)]
    source: PathBuf,
    /// Additional LVM physical volumes in any order, image segments, VMDK extents, or backing files (nearest parent first).
    #[arg(long)]
    segment: Vec<PathBuf>,
    /// Image type such as raw, ewf, vhd, vhdx or vmdk. import-image also accepts lvm2 with --lvm-volume, qed, vdi, qcow1, qcow2, dmg, cd-raw and compressed-raw. Default: auto-detect.
    #[arg(long)]
    image_type: Option<String>,
    /// Logical sector size in bytes; default is supplied by the reader.
    #[arg(long, value_parser = clap::value_parser!(u32).range(512..=65536))]
    sector_size: Option<u32>,
}

impl From<ImageArgs> for ImageInput {
    fn from(v: ImageArgs) -> Self {
        Self {
            source: v.source,
            segment: v.segment,
            image_type: v.image_type,
            sector_size: v.sector_size,
        }
    }
}
#[derive(Args)]
pub struct FilesystemArgs {
    #[command(flatten)]
    image: ImageArgs,
    /// File system type, such as ext4, hfs or ufs. Default: auto-detect.
    #[arg(long)]
    fs_type: Option<String>,
    /// Starting sector of the volume within a whole disk image.
    #[arg(long, default_value_t = 0)]
    sector_offset: u64,
    /// Restore selected file IDs from the listing.
    #[arg(long, requires = "destination", conflicts_with = "all")]
    id: Vec<String>,
    /// Restore every listed deleted regular file.
    #[arg(long, requires = "destination")]
    all: bool,
    /// Existing output files are preserved. Omit to list deleted metadata.
    #[arg(long)]
    destination: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

pub fn import(
    image: ImageArgs,
    output: &Path,
    selector: Option<disk_image::Qcow2SnapshotSelector>,
    expected: disk_image::RawExpectation,
    json: bool,
    cancel: &AtomicBool,
) -> Result<i32> {
    super::print(
        serde_json::json!(disk_image::import_with_snapshot(
            &image.into(),
            output,
            expected,
            selector,
            cancel
        )?),
        json,
    );
    Ok(0)
}

pub fn filesystem(request: FilesystemArgs, cancel: &AtomicBool) -> Result<i32> {
    let (report, failed) = disk_image::filesystem(
        FilesystemRequest {
            image: request.image.into(),
            fs_type: request.fs_type,
            sector_offset: request.sector_offset,
            all: request.all,
            id: request.id,
            destination: request.destination,
        },
        cancel,
    )?;
    super::print(report, request.json);
    Ok(if failed { 5 } else { 0 })
}

#[derive(Args)]
pub struct ApfsArgs {
    #[command(flatten)]
    image: ImageArgs,
    /// First sector of the APFS container in the logical image.
    #[arg(long, default_value_t = 0)]
    sector_offset: u64,
    /// APSB block reported by the APFS listing.
    #[arg(long)]
    volume_block: Option<u64>,
    /// Compare this snapshot transaction ID with the current directory tree.
    #[arg(long)]
    snapshot_xid: Option<u64>,
    #[arg(long, requires = "destination", conflicts_with = "all")]
    id: Vec<String>,
    #[arg(long, requires = "destination")]
    all: bool,
    #[arg(long, requires = "snapshot_xid")]
    destination: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}

pub fn apfs(request: ApfsArgs, cancel: &AtomicBool) -> Result<i32> {
    let (report, failed) = disk_image::apfs(
        disk_image::ApfsRequest {
            image: request.image.into(),
            sector_offset: request.sector_offset,
            volume_block: request.volume_block,
            snapshot_xid: request.snapshot_xid,
            id: request.id,
            all: request.all,
            destination: request.destination,
        },
        cancel,
    )?;
    super::print(report, request.json);
    Ok(if failed { 5 } else { 0 })
}

#[derive(Args)]
pub struct IsoArgs {
    /// Cooked 2048-byte-sector ISO image. Convert raw CD tracks with import-image first.
    #[arg(long)]
    source: PathBuf,
    /// Historical session number to compare with the latest session.
    #[arg(long)]
    session: Option<u32>,
    /// Exact historical file path from the candidate list. Repeat to select more.
    #[arg(long = "path", requires = "destination", conflicts_with = "all")]
    paths: Vec<String>,
    #[arg(long, requires = "destination")]
    all: bool,
    #[arg(long, requires = "session")]
    destination: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}
pub fn iso_sessions(args: IsoArgs, cancel: &AtomicBool) -> Result<i32> {
    let (result, failed) = disk_image::iso_sessions(
        disk_image::IsoRequest {
            source: args.source,
            session: args.session,
            paths: args.paths,
            all: args.all,
            destination: args.destination,
        },
        cancel,
    )?;
    super::print(result, args.json);
    Ok(if failed { 5 } else { 0 })
}

#[derive(Args)]
pub struct BtrfsArgs {
    /// Single-device RAW volume image with a valid primary Btrfs superblock.
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    current_root: Option<u64>,
    #[arg(
        long,
        requires = "current_root",
        conflicts_with = "previous_generation"
    )]
    snapshot_root: Option<u64>,
    /// Compare the same subvolume in an older transaction advertised by the superblock.
    #[arg(long, requires = "current_root")]
    previous_generation: Option<u64>,
    #[arg(long, requires = "destination", conflicts_with = "all")]
    id: Vec<u64>,
    #[arg(long, requires = "destination")]
    all: bool,
    #[arg(long, requires = "current_root")]
    destination: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}
pub fn btrfs(a: BtrfsArgs, cancel: &AtomicBool) -> Result<i32> {
    let (report, failed) = disk_image::btrfs(
        disk_image::BtrfsRequest {
            source: a.source,
            current_root: a.current_root,
            snapshot_root: a.snapshot_root,
            previous_generation: a.previous_generation,
            id: a.id,
            all: a.all,
            destination: a.destination,
        },
        cancel,
    )?;
    super::print(report, a.json);
    Ok(if failed { 5 } else { 0 })
}

#[derive(Args)]
pub struct Ext4JournalArgs {
    /// Read-only ext4 filesystem image.
    #[arg(long)]
    source: PathBuf,
    /// Inode from the historical listing.
    #[arg(long)]
    inode: Option<u32>,
    /// New file to write. Omitting this lists prior inode layouts.
    #[arg(long, requires = "inode")]
    output: Option<PathBuf>,
    /// Require the recovered bytes to match this original SHA-256.
    #[arg(long, requires = "output", conflicts_with = "allow_unverified_content")]
    sha256: Option<String>,
    /// Save bytes reconstructed from metadata without an independent original digest.
    #[arg(long, requires = "output")]
    allow_unverified_content: bool,
    #[arg(long)]
    json: bool,
}

pub fn ext4_journal(args: Ext4JournalArgs, cancel: &AtomicBool) -> Result<i32> {
    let report = disk_image::ext4_journal(
        disk_image::Ext4JournalRequest {
            source: args.source,
            inode: args.inode,
            output: args.output,
            sha256: args.sha256,
            allow_unverified_content: args.allow_unverified_content,
        },
        cancel,
    )?;
    super::print(report, args.json);
    Ok(0)
}
