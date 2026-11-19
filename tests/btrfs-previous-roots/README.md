# Real Btrfs unlinks without user snapshots

Linux created two independent, disposable 160 MiB filesystems with single data and DUP metadata. The generator wrote a UTF-8 log, CSV, BMP, WAV, ZIP, binary file and sparse file, read all seven back, committed the transaction, then unlinked each file. The current subvolume still contains a control file. The kernel reported no user snapshots. After the deletion commit the test froze and copied the loop device. It then unmounted the original and checked the acquired image with btrfs-progs. [Generator and independent measurements](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44597862024)

The primary superblock advertises transactions 6, 7, 8 and 9. Generation 7 predates the unlinks. Neither the current tree at generation 9 nor the older tree at generation 8 contains the removed paths. The independent offline command using the previous root and filesystem tree recovered 0/7 exact files from the uncompressed volume and 5/7 from the Zstandard volume in this experiment.

Rescue reads the selected transaction's chunk tree and checksum tree. This includes an older data chunk that is absent from the current chunk map. Both acquisitions recovered all seven inputs with matching bytes and SHA-256. The portable test also overwrites a source sector. The affected file is withheld and six still match. Substitution of a different root tree, chunk tree or checksum tree is refused before any output is created.

```sh
python scripts/e2e_btrfs_previous.py --binary target/release/rescue
```

Each manifest records the post-deletion image hashes and independently calculated original hashes. The tested records are recent and survived. An expired root, discarded data or an overwritten transaction can prevent recovery.
