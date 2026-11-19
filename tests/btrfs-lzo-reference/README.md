# Btrfs LZO and Zstandard layouts

For each compression mode, Linux created a Btrfs subvolume, wrote 13 originals, created a read-only snapshot, then unlinked the originals. The generator read all 13 snapshot files through the mounted filesystem and verified the bytes and SHA-256. The reference includes three inline documents/images, a private binary, a shared slice of that binary starting at offset 8192, preallocated zeros with written data, an empty file and a file containing a 1 MiB hole. Each manifest also records the results obtained with the independent offline btrfs-progs reader. The mounted Linux filesystem verified every original in both snapshots.

[Kernel fixture generation and checks](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44592303303) · [Generator](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/0aab46e/scripts/generate_btrfs_lzo_reference.py)
