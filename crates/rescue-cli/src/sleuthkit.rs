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
    /// Additional image segments in logical order, for explicitly split images.
    #[arg(long)]
    segment: Vec<PathBuf>,
    /// Image type such as raw, ewf, vhd, vhdx or vmdk. import-image also accepts standalone vdi, qcow2 and dmg. Default: auto-detect.
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

pub fn import(image: ImageArgs, output: &Path, json: bool, cancel: &AtomicBool) -> Result<i32> {
    super::print(
        serde_json::json!(disk_image::import(&image.into(), output, cancel)?),
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
