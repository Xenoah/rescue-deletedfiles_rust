# Btrfs snapshot originals

The Linux kernel created two disposable 160 MiB, single-device Btrfs volumes. For each volume it created a subvolume, copied seven corpus files into it, took a read-only snapshot, unlinked all seven originals, and created one later file. It verified every historical byte and SHA-256 and the disappearance of the current paths. The plain volume also passed extraction with btrfs-progs. The zlib volume contains inline file data compressed with zero padding out to a 4096-byte sector.

Both btrfs-progs on Ubuntu 24.04 and btrfs-progs 6.14 returned an inflate error on the compressed inline PNG during offline extraction. The kernel mounted the same snapshot and read the correct bytes. The `btrfs` subcommand uses the system chunk array, chunk tree and its own checked inline decoder to recover all seven files from each volume.

[Independent native generation and validation](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44588576081) · [Fixture generator](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/d01d259/scripts/generate_btrfs_reference.py)
