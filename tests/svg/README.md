# SVG and SVGZ recovery fixtures

The eight source artworks contain a rectangle, a gradient, a clipping path, reused geometry, a transformed curve, Unicode text in metadata, an Inkscape plain-SVG export and a Cairo-generated path. Each has a corresponding single-member gzip SVGZ. The 32 originals include an XML declaration, a UTF-8 BOM, a prefixed SVG root, UTF-16LE/BE documents with matching BOMs and files beginning directly with the root. The reference manifest records each whole file's byte count, SHA-256 and the decoded 32 × 24 RGBA raster from librsvg 2.60.0. Native checks render the original and recovered file with the same installed renderer and compare all RGBA bytes by SHA-256.

The test deletes all 32 files with mtools from FAT32 and with debugfs from ext4, then recovers each set with normal and indexed searches. Fourteen plain SVGs and sixteen SVGZs have independently delimited whole-file matches. Cairo and Inkscape add a line feed after the root. The carved prefixes end at the closing tag and render identically. Filesystem metadata recovers the exact full lengths of those originals. FAT32 uses explicit contiguous estimation for multi-cluster documents, including the Inkscape SVG and the UTF-16 boundary artwork. The default Pillow-based bitmap preview does not render vector files.

```sh
python scripts/e2e_svg.py --binary target/release/rescue --rsvg-convert /usr/bin/rsvg-convert --output results.json
```

To regenerate the artwork in a new directory, run `python tests/svg/capture.py --output regenerated-svg` with Inkscape, librsvg, Pillow and a Python interpreter with pycairo.

Without the optional renderer, the suite uses an adjacent-file RAW including a scan-window boundary and compares source bytes and hashes. Damage cases include invalid namespaces, mismatched XML tags, undeclared entities, gzip CRC/length damage and missing acquisition ranges. Both typed records still recover byte-for-byte when an unread sector starts exactly after their final byte. A changed but well-formed color value still produces a valid document with a different SHA-256. A generic XML or gzip search can still be selected explicitly, and previously recorded generic candidates retain their original extension.

The UTF-16 fixtures exercise both byte orders, explicit endian declarations, unprefixed and prefixed roots, a supplementary Unicode character and a surrogate pair crossing a 4 KiB read boundary. The markup reader obtains the original byte offsets, then the full UTF-16 document is decoded strictly and its real Unicode element names and namespaces are checked.
