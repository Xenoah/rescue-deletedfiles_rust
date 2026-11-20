# VirtualBox VMDKs with UUID-linked parents

Oracle VirtualBox 7.2.22 created the base, child and grandchild VMDKs by replaying the captured Windows NTFS guest sectors from [dynamic-flushed](../vhd-chain/dynamic-flushed). Every generation was read back through VirtualBox and compared to the complete Windows guest. VirtualBox writes `parentCID=ffffffff` and omits `parentFileNameHint` even in these children. Its `ddb.uuid.parent` identifies the required parent.

The Linux packages of v1.33.0, v1.34.0 and v1.35.0 previously accepted each child alone. The apparent success omitted inherited bytes. Both 96 MiB outputs differed from their complete guests at 4,065,178 byte positions. The measured executable and output SHA-256 values are in [the public-package comparison](previous-public-results.json).

Rescue now refuses these child-only imports in automatic and explicit VMDK mode. The true standalone base still matches the complete guest. The independent test opens all three explicit members with VirtualBox's read-only disk API, checks every byte, writes the complete guest to RAW, and recovers seven actual Windows unlinks through both ordinary and indexed Rescue scans. Regeneration uses `python tests/vmdk-vbox-parent/capture.py --vbox-img /path/to/vbox-img --output /path/to/empty-folder`. Do not convert a child alone to RAW with software that ignores its parent UUID.
