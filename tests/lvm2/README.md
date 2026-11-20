# Linux LVM2 logical volumes

These are complete, compressed images of three volume groups created with Debian LVM 2.03.31 and device-mapper on Linux 6.12.107. Each group contains two 32 MiB physical volumes, two 16 MiB logical volumes, and direct copies of the logical devices read from the running kernel after the deletions. The manifests record the kernel's mapping tables, the SHA-256 of every physical and logical disk, and each deleted file's SHA-256. Decompressing the XZ fixtures requires only the Python standard library.

| Group | Linear LV | Striped LV | Metadata |
| --- | --- | --- | --- |
| extent8-stripe64 | 8 MiB on the first PV, 8 MiB on the second | two 64 KiB stripe legs | one metadata area on each PV |
| dual-mda-extent3-stripe16 | 3 MiB on the first PV, 13 MiB on the second | two 16 KiB stripe legs | two copies on each PV, including the last MiB |
| one-mda-stripe4 | 16 MiB on the PV with zero metadata copies | two 4 KiB stripe legs | one active metadata area for the entire group, after 27 tag updates |

Each LV was formatted as ext4, mounted in an isolated QEMU guest and populated with GIF, JPEG, PDF, PNG, SQLite, WAV and ZIP originals from `tests/corpus/files`. For the third group an additional 13 KiB PNG spans several stripes. The guest ran `cmp` on each copy, removed the files through the mounted filesystem, synced, unmounted and used `dd` to a separate empty disk to capture the logical volume as served by the kernel. Each output disk's digest was compared with that of its device-mapper source. The additional 4 MiB allocation on the second group's linear volume put the deleted files after the 3 MiB PV boundary.

Run the standalone recovery test from the repository root.

```sh
python scripts/e2e_lvm2.py --binary target/release/rescue --output lvm2-results.json
```

The suite checks both orders of PV paths, selects LVs by name and UUID, compares **every** logical byte with the kernel output, and rescans that output with ordinary and indexed carving. Invalid cases include bad PV and metadata checksums, a divergent mirror, missing or duplicate PVs, invalid ranges, overlapping physical allocations, unsupported snapshot/thin/RAID mappings and a valid data modification detected only by a known hash. No LVM software or kernel device-mapper privileges are needed to run these tests or to recover the supplied images.
