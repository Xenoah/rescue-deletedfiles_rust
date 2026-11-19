# APFS snapshot fixture

`snapshot.raw.xz` contains one disposable, unencrypted APFS volume. Seven files in `Rescue-Test` were written, snapshotted and unlinked. One file survived in both directory trees and another was created after the snapshot. `manifest.json` records the reference disk hash, compressed hash, original file hashes, volume block and snapshot XID.

The independent generator used the Linux APFS read/write module and apfsprogs. It mounted the historical snapshot read-only, checked all seven original hashes, then read them again with The Sleuth Kit. Apple's native APFS reader independently mounted the current volume and the snapshot read-only in macOS and verified the same seven originals. [Generator and verification run](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44580072513) · [Native Apple verification](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44582256202)

To run Rescue against this fixture with the APFS-enabled Sleuth Kit on PATH, use `python3 scripts/e2e_apfs_snapshots.py --binary /path/to/rescue`. On macOS, `python3 scripts/apfs_native_reader.py` independently mounts and reads the two namespaces.
