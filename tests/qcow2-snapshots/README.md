# QCOW2 internal disk snapshots

These five images were written by QEMU 10.0.13 from the complete guest disks in the [Windows NTFS](../vhd-chain/) and [FAT32 and ext4](../vdi-chain/) fixtures. The guest operating system or filesystem tool deleted seven real files in each final saved generation. The two ext4 fixtures preserve all eight generations with zero through seven deletions. The active disk has a later 512-byte write. Its SHA-256 differs from every saved disk.

The fixtures exercise v2 and v3, 4/64/128 KiB clusters, Zstandard compression, extended L2 subclusters, L2 tables shared between historical views, Unicode snapshot names, and distinct active and historical data. The test lists the directory and selects each of the 25 saved disks independently by ID and by name. Each export is compared directly with its independently captured whole guest and SHA-256. The ordinary and indexed recovery paths compare the actual recovered file bytes with the original corpus. Negative cases cover ambiguous names, repeated IDs, overlapping directory/L1/L2/data regions, truncated metadata, saved memory and unknown selectors. The logical SHA-256 option also detects a changed data byte in an otherwise valid image.

```sh
python scripts/e2e_qcow2_snapshots.py --binary target/release/rescue --qemu-img /usr/bin/qemu-img --output snapshots.json
python tests/qcow2-snapshots/capture.py --output new-fixtures
```

The optional native check converts every saved generation again with QEMU and checks ext4 deletion history with The Sleuth Kit. Generating new fixtures requires qemu-img and qemu-io. Snapshot timestamps change on regeneration. Archived images and per-generation expected hashes in this directory are the fixed inputs to the portable tests.
