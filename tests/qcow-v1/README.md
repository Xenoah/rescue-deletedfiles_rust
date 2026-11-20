# Original QEMU QCOW v1 reference

The XZ stream holds a standalone QCOW v1 capture of a FAT32 filesystem. Seven files were copied onto the disk, read back, and deleted with mtools. Extra guest sectors after the filesystem exercise a non-compressible cluster, an unallocated table and a final partial cluster. QEMU created the disk and issued compressed writes with qemu-io. QEMU expanded the result and the test compared the entire guest with the original RAW. The manifest records the two disk hashes, mapping counts and seven original file hashes.

Run the portable comparison with `python scripts/e2e_qcow1.py --binary target/release/rescue`. The `--native` option also creates sparse and compressed QCOW v1 captures of real FAT32, ext4 and Windows NTFS deletions. To create a fresh reference in an empty directory, use `python scripts/e2e_qcow1.py --write-reference <directory>`.
