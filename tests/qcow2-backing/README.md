# QCOW2 backing-chain reference

QEMU created this three-member chain from a FAT32 disk on which seven files were written, read back and deleted with mtools. The youngest member uses extended L2 entries, the middle member uses standard L2 entries, and the standalone base uses Zstandard compression. The older versions deliberately contain different bytes at two deleted files and nonzero data under a subsequent zero write. QEMU reported reads from all three depths. Its complete logical output matched the original FAT32 disk byte for byte.

The manifest records the original file hashes, expected disk hash and SHA-256 of each uncompressed member. The portable test works without QEMU. To regenerate the fixture using current filesystem and QEMU tools, run the following command from the repository root. A new filesystem has its own creation time and volume ID.

```sh
python scripts/e2e_qcow2_backing.py --binary target/release/rescue --native --write-fixture tests/qcow2-backing
```
