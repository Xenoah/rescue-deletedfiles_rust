# Btrfs snapshot data extents

Linux wrote seven deterministic originals to a single-device volume with duplicate metadata, created a read-only snapshot, and unlinked the seven live paths. It repeated the process with forced zlib and Zstandard compression. The seven originals are UTF-8 text, CSV, BMP, PCM WAV, ZIP, incompressible private binary, and a sparse file with two data regions around a 1 MiB hole. The generator compared both the mounted snapshots and offline btrfs-progs extraction to the original bytes and SHA-256.

The manifest records independently measured physical addresses for the two snapshot root mirrors and the first regular file's first data sector. The portable CLI test changes these bytes only in temporary copies. It verifies all seven files with either a healthy volume or a lost metadata mirror, and confirms the reported per-file checksum failure on damaged data.

[Fixture generator](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/a61b2a5/scripts/generate_btrfs_large_reference.py) · [Independent native validation](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44591328958)
