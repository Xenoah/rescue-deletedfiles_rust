# Linux device-mapper persistent snapshots

Linux 6.12.107 and LVM 2.03.31 created these complete physical-volume images in isolated QEMU guests. Each guest formatted and mounted an ext4 logical volume, wrote eight originals in seven file formats, compared the copies and unmounted it. It created a snapshot called `before`, remounted the origin, unlinked the eight files, unmounted and created a second snapshot called `deleted`. The guest then overwrote 10 MiB of the origin and wrote another copy of `noise.png` at a new offset. Direct reads of the activated device-mapper devices provided the independent whole-disk oracles.

| Group | Origin and COW layout | Preserved conditions |
| --- | --- | --- |
| striped-origin | Striped origin, linear stores on different PVs | 8 KiB and 64 KiB chunks. The first store has exception entries across three metadata areas. |
| striped-store | Origin split across two PVs, one striped store and one split store | 4 KiB and 16 KiB chunks. Each store was created separately and attached with `lvconvert --type snapshot`. The LVM-reported UUIDs and allocation sizes are included in the manifest. |
| exhausted-store | Striped origin, four snapshots | Linux invalidated a 1 MiB store after it filled. A new 32 KiB-chunk snapshot shares all logical bytes with the current disk. Linux successfully read the other two historical views and refused to read the exhausted one. |

Each manifest records the kernel's mapping tables, full disk digests, original files recovered from each view and the native kernel status. Capture images are XZ-compressed. The eight distinct originals are GIF, JPEG, PDF, two PNGs, SQLite, WAV and ZIP from `tests/corpus/files`.

```sh
python scripts/e2e_lvm2_snapshots.py --binary target/release/rescue --native --output results.json
```

Both ordinary and indexed scans restore all eight original files from each post-unlink snapshot. From each pre-unlink snapshot the files are still live. In the overwritten current views the only matching file is the separately written PNG. Native verification uses `fls` and `icat` to pair the original path and inode with the complete live bytes, then `fls` and `istat` verify the same unallocated inodes and zero link counts after unlink. The deleted names appear as anonymous orphan records in these ext4 snapshots. The portable check compares the complete exported disks to the kernel readbacks and verifies every recovered byte and SHA-256.

Corrupt-store tests include an invalidated header, different chunk size, duplicate or reserved chunk references, offsets beyond the origin or store, corruption in a later metadata area, broken dependencies and a valid data alteration detected by the known whole-disk digest. A healthy sibling and the origin remain independently readable after damage confined to a separate store. Extraction never activates device-mapper devices or writes to the PVs.
