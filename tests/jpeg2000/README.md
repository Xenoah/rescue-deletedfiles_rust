# JPEG 2000 sample provenance

`generate.py` creates ten distinct images with Pillow/OpenJPEG and FFmpeg's native JPEG 2000 encoder, plus five JP2 container variations around their codestreams. The manifest records the encoded SHA-256, dimensions, mode, and SHA-256 of the independently decoded pixel bytes.

Apple ImageIO produced the six `apple*.jp2` originals on macOS 26. The capture decoded each saved file with both ImageIO and OpenJPEG and retained their separate pixel digests. It also recorded the source PNG and Apple's readback PNG. A Linux export job checked every archived JP2 against that manifest. [Apple generator](https://github.com/Xenoah/rescue-deletedfiles_rust/blob/2fca466bffbc841e6d04abd906d4ac27cb0dfaa4/scripts/qa_apple_jp2.py) · [Native capture](https://github.com/Xenoah/rescue-deletedfiles_rust/actions/runs/44672922457) · [Reference originals and readbacks](https://github.com/Xenoah/rescue-deletedfiles_rust/tree/25e7078/reference/apple-jp2)

Run `python scripts/e2e_jpeg2000.py --native --binary /path/to/rescue` on Linux to create FAT32 and ext4 images, delete the originals, and compare both recovery modes with their bytes, SHA-256 and decoded samples. The ICC cases also compare color-managed sRGB samples from original and recovered files using LittleCMS.
