# Dependency license sources

Some published crates omit their repository-level license files.
The files under licenses/ are copied from the exact upstream commits recorded
in those crates' .cargo_vcs_info.json. licenses.json records the package versions,
upstream URLs, and SHA-256 checksums.

scripts/notices.py collects license text for the actual Windows normal/build
dependency tree, checks the vendored text, and includes embedded-font notices
from the verified Cargo registry packages. Unused optional dependencies are not
part of the distributed executable. No third-party executable code is vendored here.
