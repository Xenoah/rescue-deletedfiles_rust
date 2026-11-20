# Native QEMU Enhanced Disk references

QEMU 6.2.0 (Ubuntu) and 10.0.13 (Debian) each converted three captured guest disks to standalone QED and independently converted them back to RAW. The Windows NTFS source contains seven files flushed before Windows deleted them. The mtools FAT32 disk contains seven deletions. The Linux ext4 snapshot followed eight actual unlinks. The original logical captures and the complete expected file contents already live in the repository. Each manifest records the input reference, creation tool, container SHA-256 and logical SHA-256.

The six standalone images include 4–128 KiB clusters, 2–16 clusters per table, sparse L1 and L2 entries, allocated zero data and a QEMU-created explicit zero cluster. One 16 MiB ext4 disk has an additional nonzero 512-byte sector after its filesystem to exercise a partial final cluster.

The two three-member histories were created with `qemu-img create -b` and populated with `qemu-io` writes of only the changed clusters. `windows-raw` starts with a Windows RAW disk explicitly marked as RAW by QEMU. In `short-middle`, the youngest and oldest QED disks are longer than the intermediate QED. A small witness area appended outside the ext4 filesystem includes base data, middle data, a youngest-layer explicit zero hiding nonzero parent data, a hole beyond the middle disk and a youngest-layer data write. QEMU independently compared each view to the source and read back the entire logical disk. All files are compressed for storage and are tested as regular files.

```sh
python scripts/e2e_qed.py --binary target/release/rescue --native --output qed-results.json
```

The native check additionally uses QEMU's read-only check, map and conversion commands. The Rescue import runs with an empty PATH. The suite compares actual bytes and SHA-256 in the full logical disks and in files recovered with both scan modes. It rejects malformed tables, invalid references, dirty images, missing parents, untyped RAW parents and changed contents when an original digest is known. The caller supplies every parent. A matching name cannot authenticate the original contents.

The separate `gpt-4g` fixture is a 4,311,744,512-byte sparse guest. GNU Parted created its GPT. The ext4 partition was placed so four deleted files start below the 4 GiB logical address and four above. Its payload is the existing Linux device-mapper capture. Only the QED container and the GPT sectors are stored here. On Linux, the following additional test reconstructs a sparse reference, compares all logical bytes and SHA-256, checks QEMU's comparison and The Sleuth Kit's partition listing, and recovers the eight originals with both search modes.

```sh
python scripts/e2e_qed_boundary.py --binary target/release/rescue --output boundary.json
```
