# Windows VHDX reference images

Windows Server 2022 created separate, disposable 96 MiB fixed and expandable VHDX disks, formatted them with NTFS, wrote and read back seven corpus files, and unlinked the seven names. After dismounting, the test attached each guest read-only with the Windows storage driver and hashed the complete physical disk in 96 one-MiB reads. The manifest records the full guest hash, every block hash, and compressed and uncompressed container hashes.

Linux QEMU independently reproduced the complete Windows guest and every one-MiB digest. [Fixture workflow and actual run](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44606999126).

The portable test decodes the preserved containers with the helper program path emptied, scans the resulting NTFS partitions, and compares all seven recovered files with the checked-in originals. It also checks damaged and conflicting header and region copies, overlapping and invalid allocations, active logs, and parent requirements.

The `4kn` fixture uses the Windows CreateVirtualDisk API, virtual disk version 2, with both sector sizes explicitly set to 4096 bytes. Windows verified the mounted disk geometry, created GPT and NTFS, unlinked the same seven files and hashed the complete reattached read-only guest. The native reader, libvhdi/TSK and the Windows driver agree on every guest byte by SHA-256. Both Rescue scan modes restore the original seven files. Independent fls and icat check the deleted NTFS records and contents on the libvhdi output. The NTFS uses 1024-byte MFT records with two 512-byte fixups. [Native 4Kn fixture run](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44608403126).

A sparse 4,361,292,800-byte guest relocates the same Windows NTFS partition so three of its deleted file records lie before the 4 GiB boundary and four begin at or after it. QEMU writes a standalone VHDX with 1 MiB payload blocks. The native import checks the intervening sector-bitmap slot, reproduces the full guest SHA-256, and recovers all seven originals. The test also corrupts that slot and verifies refusal.
