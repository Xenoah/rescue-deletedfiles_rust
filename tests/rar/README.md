# RAR archive recovery

These archives contain files from the project's [original-file corpus](../corpus/files). Five were created with RAR 6.23 in RAR 4 format and five with RAR 7.23 in RAR 5 format. The writers are supplied by RARLAB. Only the archives and their source manifest are stored here.

The test writes every archive to FAT32 and ext4, reads the archives back before deletion, deletes them with mtools or debugfs, and restores them with ordinary and indexed carving. It compares each complete archive to its original bytes and SHA-256. UnRAR independently tests the restored archive and decompresses every member to stdout for a second comparison with the original member bytes and SHA-256. The fixtures cover stored and compressed entries, solid archives, Unicode filenames, CRC32, BLAKE2sp and RAR5 quick-open headers. The Linux measurements include 10 actual deletions on each filesystem and 29 member files across the 10 archives.

```sh
python scripts/e2e_rar.py --binary target/release/rescue --unrar /usr/bin/unrar --output rar-native.json
```

Without the independent reader, the test carves all ten adjacent archives from a raw image on any supported platform. It also checks unsupported encrypted, split and commented archives, damaged headers and compressed or stored data, truncation, and an unread sector next to an intact archive. The file names inside an archive are used only by the independent comparison. Rescue validates decoded bytes without writing member paths.

`capture.py` regenerates the ten complete archives using explicitly supplied RAR 6 and RAR 7 writers and an UnRAR reader. The RAR 6.23 writer came from Debian's `rar_6.23-1~deb11u1_amd64.deb` (SHA-256 `a11852bf71ff9825879eea6ad1e483de03c4a9eab3909305d3df16a2fb5439ac`). The RAR 7.23 writer and UnRAR 7.23 came from RARLAB's `rarlinux-x64-723.tar.gz` (SHA-256 `759b4b6aa0d9f77131882162951193f3a0e54bf60e1d8dc4255aa308accab588`). The native Linux recovery test also ran with Ubuntu 22.04 UNRAR 6.11 beta 1 freeware (package 6.1.5-1).

The structural reader supports contiguous single-volume RAR 4/5 archives with a valid end record and integrity-checked file members. It checks every decoded file's CRC32 or BLAKE2sp and compares stored quick-open records to the referenced headers. Current bounds are 64 MiB of archive and decoded member data, 4096 headers, a 32 MiB RAR5 dictionary and a 128 MiB decoder workspace allowance. Archive comments, recovery records, encrypted or split sets and unsupported service records need the corresponding original volumes, password, and a compatible archive tool. A signature or checksum cannot reconstruct missing, overwritten or trimmed sectors.
