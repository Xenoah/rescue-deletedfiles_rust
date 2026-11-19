# Netpbm image fixtures

ImageMagick 7.1.1-43 Q16 generated these nine images. The manifest records the arguments following the executable, dimensions, magic and SHA-256 for each original. Redirect standard output to the named file to regenerate using that build.

The portable integration suite covers P1 through P7 and malformed headers, raster boundaries, oversized dimensions and invalid samples. On Linux the native suite writes and reads each original on an ext4 image, deletes all nine entries with debugfs, confirms them with fls, and compares bytes and SHA-256 after recovery. FFmpeg separately decodes each original and recovered file to RGBA64 big-endian and compares every channel.
