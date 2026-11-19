# Standalone VMDK reference images

QEMU converted two independently measured Windows guests containing seven genuine NTFS unlinks each. The logical SHA-256 values and original files come from the [Windows reference images](../windows-vhdx/README.md). Both checked-in VMDKs use hosted monolithic sparse extents with redundant grain metadata. In the second image QEMU enabled explicit zero grains and qemu-io wrote a full zero grain at 60 MiB over independently verified existing zeros.

The portable test removes external reader programs from the search path, imports each renamed container, compares the full Windows guest hash and all 96 one-MiB hashes, and checks the bytes and SHA-256 of seven recovered files. The Linux test also compares the VMDK directly with the resulting RAW using qemu-img.
