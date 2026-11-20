# QCOW2 v3 with extended L2 entries and Zstandard

The fixture contains a 40 MiB FAT32 volume with seven real mtools deletions. The source files are in `tests/corpus/files`. QEMU compressed the volume with 16 KiB clusters. An additional cluster outside the filesystem contains independently written data in subclusters 0 and 31, an explicit zero in subcluster 1 and unallocated subclusters. Two host subclusters contain 0xa9 behind the explicit-zero and unallocated bits. QEMU's independent RAW export and Rescue's import both return zero in those regions.

The manifest records the QEMU version, logical length, exact logical SHA-256 and the individual originals. The separate Zstandard frame was encoded with the zstd CLI and includes a checksum. It decodes to exactly one cluster of byte 0x51.

Regenerate in a temporary environment with QEMU, mtools, dosfstools and zstd on PATH. Recreating FAT32 chooses a new volume ID, so new images can have different disk digests. Each generation gets its digests from that disk.

```sh
python scripts/e2e_qcow2_subclusters.py --write-reference tests/qcow2-subclusters
python scripts/e2e_qcow2_subclusters.py --binary /path/to/rescue
python scripts/e2e_qcow2_subclusters.py --native --binary /path/to/rescue
```

The portable test reads the recorded image using the supplied executable with an empty PATH. The native test also constructs new FAT32, ext4 and Windows NTFS images and verifies them against qemu-img and the original deleted files.
