# Native Btrfs checksum fixtures

Linux formatted three disposable Btrfs volumes with XXH64, SHA-256 and BLAKE2b-256. Each uses Zstandard data compression and duplicate metadata on one device. The test created a read-only snapshot, unlinked seven files in the live subvolume, and checked both subvolumes using the kernel and btrfs-progs. The originals are a text log, CSV, BMP, WAV, ZIP, binary file and a sparse file. [Linux reference generation and independent recovery](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44599231476)

The executable recovers each original byte-for-byte. The original SHA-256 values are independent of the checksum algorithm used by the filesystem. Each manifest also records the physical mirrors of the snapshot leaf and the first data sector of the log file, as reported by btrfs-progs. On a writable test copy, the test damages one metadata mirror, both mirrors, a superblock checksum and a file sector. The surviving mirror supplies a valid tree after a single-copy error. A damaged data sector prevents publication of the affected file while the other six still match.

```sh
python scripts/e2e_btrfs_checksums.py --binary target/release/rescue
```
