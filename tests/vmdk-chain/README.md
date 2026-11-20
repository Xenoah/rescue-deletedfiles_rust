# Recovery from real VMDK snapshots

The QEMU fixtures contain hosted sparse images and a compressed streamOptimized base created by QEMU 6.2 and 10.0. Each child contains only the grains that changed between independently captured logical disks. The three Oracle VirtualBox 7.2.22 VMDKs are shared with the [UUID-only parent regression](../vmdk-vbox-parent). Their parent CID is `ffffffff` and their descriptors contain no parent filename hint.

| Producer and original guest | Generations | Deletions recovered from the final snapshot |
| --- | ---: | --- |
| QEMU 10, Windows NTFS | 3 | AVI, DOCX, FLAC, PPTX, SQLite, TAR, WAV |
| QEMU 10, compressed Windows NTFS base and sparse children | 4 | The same seven Windows deletions, including a 32 KiB child over a 64 KiB compressed base |
| Oracle VirtualBox 7.2.22, same Windows NTFS | 3 | The same seven Windows deletions |
| QEMU 6.2, FAT32 | 3 | BMP, GIF, PDF, PNG, WAV, WebP, ZIP |
| QEMU 10, ext4 | 8 | The same seven formats, one additional unlink per generation |

Every generation was imported with no external executable on PATH. The full output length, bytes and SHA-256 were compared to the original guest. The independent Linux tests open all eighteen QEMU generations with QEMU and the three Oracle images with VirtualBox. The final guest of every chain was scanned in ordinary and indexed modes, and all recovered files were compared to the original file bytes and SHA-256. The Linux ext4 metadata tests also checked the names, inode numbers and bytes for each of the 28 cumulative unlinks. The FAT32 middle generation contains 16 explicit zero grains over an allocated parent.

The portable tests supply missing, reordered, unrelated and stale parents, malformed or conflicting IDs and unsafe filename hints. Altered inherited grain contents retain a valid identity and can change the flattened disk. The known original guest SHA-256 makes Rescue refuse this output before publication.

To reproduce with the system QEMU reader and an extracted or installed VirtualBox user-space reader on Linux, run the following command. The filesystem-metadata comparison also uses The Sleuth Kit.

```sh
python scripts/e2e_vmdk_chain.py --binary target/release/rescue \
  --qemu-img /usr/bin/qemu-img --vbox-img /usr/lib/virtualbox/vbox-img \
  --output vmdk-chain-results.json
```

Omit both reader arguments to compare all 21 generations against the stored guest captures on any platform. To generate new QEMU fixtures, run `python tests/vmdk-chain/capture.py qemu6-fat32 NEW_DIRECTORY` with the intended QEMU tools on PATH. QEMU generates new CIDs on each capture. The guest hashes are deterministic. The Windows disk captures and deleted originals are available in the [Windows capture](../vhd-chain/dynamic-flushed) and the [FAT32 and ext4 captures](../vdi-chain).
