# VMware descriptors rewritten by QEMU

The eight immutable examples were created by converting the Windows NTFS guest from `tests/windows-vhdx/fixed` with Ubuntu 22.04 QEMU 6.2.0 and Debian QEMU 10.0.13. The guest contains seven actual NTFS deletions. For each version we captured monolithicFlat, monolithicSparse, twoGbMaxExtentFlat and twoGbMaxExtentSparse. Each conversion was checked with that QEMU version's independent image comparison against the original RAW.

```sh
qemu-img convert -f raw -O vmdk -o subformat=twoGbMaxExtentFlat windows.raw volume.vmdk
qemu-img compare -f vmdk -F raw volume.vmdk windows.raw
```

QEMU picks a random CID. When its updated CID is shorter than the original, the NUL can be followed by a short suffix of the descriptor's last line. The manifest records the exact CID and suffix for each capture. Images ending in an ordinary zero pad are already covered by the standard VMDK tests. Flat extent files with the same SHA-256 as the original guest are represented by that guest. All other container members are stored verbatim in XZ. The sparse monolithic examples retain their embedded descriptors. All eight can be tested without installing QEMU.

These eight small disks each have a 96 MiB guest. The two split subformats each use one component at this size. Larger multi-component images are exercised separately by `scripts/e2e_vmdk_boundary.py`. [QEMU 6.2 descriptor update](https://github.com/qemu/qemu/blob/v6.2.0/block/vmdk.c) · [QEMU 10 descriptor update](https://github.com/qemu/qemu/blob/v10.0.0/block/vmdk.c)

The separate `boundary` fixtures each have two components sharing a 2,213,809,152-byte guest. The first component covers exactly 2 GiB, with three deleted NTFS records before and four at or beyond the split. After creating the images, QEMU wrote an unchanged zero sector at byte 512. A five-digit CID left a two-byte quote and newline after the NUL. Both captured descriptors, the QEMU 10 sparse extents and the hashes of both QEMU 6 flat extents are recorded. The Linux test regenerates the flat extents from the known guest, verifies their hashes, and checks the reconstructed disk against both the guest's full byte stream and the QEMU driver. [Full-disk results](../../docs/vmdk-cid-boundary-results.json)
