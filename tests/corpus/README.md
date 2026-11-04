# Synthetic format corpus

35 format families / 36 files. All contents were generated for recovery testing;
none contain user data or third-party sample content.

`manifest.json` records each file's SHA-256, byte length, generator context and
independent reader used to validate it. `noise.png` is an additional multicluster
PNG that distinguishes contiguous carving from unsupported fragment reconstruction.
`sample.apng` has two frames; extracting its original bytes through filesystem
metadata is distinct from APNG carving, which is currently unsupported.

Generate or refresh with `python scripts/make_format_corpus.py --output tests/corpus`.
The optional generator needs Pillow, reportlab, python-docx, openpyxl,
python-pptx, PyMuPDF and FFmpeg/FFprobe. The recovery runner
`scripts/e2e_formats.py` uses only Python's standard library and the committed
corpus. Git treats every fixture as binary to preserve CRLF and all exact bytes.

ZIP/Office, PDF, images, media, structured text, archives and SQLite are reopened
or decoded during generation. HTML and RTF use the explicitly recorded inspection
level, rather than claiming full application rendering. Byte-exact recovery of a
small generated sample is not a general recovery-rate benchmark.
