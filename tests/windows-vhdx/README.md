# Windows VHDX reference images

Windows Server 2022 created separate, disposable 96 MiB fixed and expandable VHDX disks, formatted them with NTFS, wrote and read back seven corpus files, and unlinked the seven names. After dismounting, the test attached each guest read-only with the Windows storage driver and hashed the complete physical disk in 96 one-MiB reads. The manifest records the full guest hash, every block hash, and compressed and uncompressed container hashes.

Linux QEMU independently reproduced the complete Windows guest and every one-MiB digest. [Fixture workflow and actual run](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44606999126).

The portable test decodes the preserved containers with the helper program path emptied, scans the resulting NTFS partitions, and compares all seven recovered files with the checked-in originals. It also checks damaged and conflicting header and region copies, overlapping and invalid allocations, active logs, and parent requirements.
