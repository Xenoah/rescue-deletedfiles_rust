# Historical ext4 extent trees

The Linux reference generator wrote seven originals with alternating data and zero ranges. WAV, BMP, PGM, PPM, ZIP, TAR and an opaque binary were read back before deletion. Linux filefrag reported 16 or 17 allocated extents for each. Each post-deletion image has an 8 MiB JBD2 checksum-v3 journal and a surviving control file. [Independent kernel experiment with 1 KiB and 4 KiB blocks](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44602142091)

Rescue reads the external extent index or leaf from the selected historical transaction, or a preceding contiguous committed transaction that supplied the block. The block must pass the owning inode's extent checksum. An unavailable or revoked block prevents recovery from that layout. For the 1 KiB fixture three inodes refer to an external tree recorded in the preceding transaction. All seven files on both volumes matched their independently generated original bytes and SHA-256.

```sh
python scripts/e2e_ext4_journal_extents.py --binary target/release/rescue
```

The test also alters an external extent checksum while preserving the enclosing journal checksums. A separate case removes the earlier journal descriptor and confirms that the reader withholds all candidates whose trees are missing.
