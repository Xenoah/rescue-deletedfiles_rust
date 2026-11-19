# GNU tar reference archives

GNU tar created these archives using ustar, GNU and POSIX/PAX formats. The files cover regular members, empty files, directories, hard links, symbolic links, extended Unicode names and GNU long names. Seven have 20-block records. The other two use blocking factors of one and seven. Every block is 512 bytes. The manifest contains each archive's original SHA-256 and the SHA-256 of the original regular files and link targets. GNU tar listed every archive and Python tarfile independently checked its contents.

The native test writes seven originals to FAT32 with mtools, reads and compares them before deletion, deletes their directory entries with mdel, and checks that the live paths disappear. The normal and indexed scanners independently recover the deleted archives.

In a separate ext4 trial, e2fsprogs stored zero-filled parts as sparse holes. For the 10 KiB ustar-text.tar, only the first 1 KiB block had a physical mapping. After seven real unlinks, the contiguous scanner found one structurally valid 30 KiB archive. None of the seven original byte streams matched. TAR lacks member-data checksums and a unique archive length beyond its end markers. Filesystem mappings or independent original checksums matter in this case.
