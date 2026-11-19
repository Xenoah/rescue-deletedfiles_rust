# Linux ext4 journal with real deletions

Linux formatted two 96 MiB volumes with 1 KiB and 4 KiB blocks, 256-byte inodes, ordinary ordered-data mode, metadata checksums, and an 8 MiB internal JBD2 checksum-v3 journal. The generator wrote, synchronized and read back seven original files, unlinked them, committed the deletion, froze the filesystem and acquired the post-deletion loop device. It independently confirmed that the current directory contains only a control file. [Kernel generation and original hashes](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44600026818)

The current deleted inodes have size zero. Rescue verifies retained journal descriptors, payload tags and commit blocks, checks the old and current inode identities, and reconstructs file offsets from the most recent surviving pre-deletion inode. The corresponding transactions are 2 and 3. The application receives no saved file layout or list of original inode numbers when producing its inventory.

All seven originals (UTF-8 log, CSV, BMP, WAV, ZIP, binary and sparse file) were recovered on both volumes with independent byte and SHA-256 comparisons. The test exercises both original-digest enforcement and explicit metadata-only recovery. It also tests wrong digests, modified descriptors and sector data, reallocated blocks, missing selections and existing destinations. ext4 stores metadata checksums but normally has no checksum of the complete user file. A write to an unallocated sector was detected only with an independent original digest.

```sh
python scripts/e2e_ext4_journal.py --binary target/release/rescue
python scripts/e2e_ext4_journal.py --binary target/release/rescue --independent
```

The optional independent test uses e2fsprogs debugfs on a temporary copy to mark a real data block allocated. The native reader refuses that extent. Neither the application nor the portable test requires e2fsprogs.
