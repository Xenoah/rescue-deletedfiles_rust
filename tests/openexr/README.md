# Native OpenEXR references

The OpenEXR C++ 3.5.4 Python binding and FFmpeg generated these flat HDR images from numeric samples. The manifest records each file SHA-256, producer, dimensions, type and decoded pixel SHA-256 for every named channel. Channel digests cover little-endian pixel values in row order, including arbitrary channels such as object ID and depth.

The recovery test writes and reads each original on FAT32 and ext4, deletes it, and compares the recovered bytes and SHA-256 in ordinary and indexed carving. On Linux it also decodes every recovered channel with OpenEXR. The portable suite uses the stored files without requiring the native decoder. For regeneration, run `python tests/openexr/generate.py /path/to/empty-directory` with OpenEXR, NumPy and FFmpeg installed.
