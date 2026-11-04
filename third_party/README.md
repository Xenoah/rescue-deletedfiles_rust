# Dependency license sources

Some published crates omit their repository-level license files.
The files under licenses/ are copied from the exact upstream commits recorded
in those crates' .cargo_vcs_info.json. licenses.json records the package versions,
upstream URLs, and SHA-256 checksums.

scripts/notices.py collects license text for the selected target's normal/build
dependency tree, checks the vendored text, and includes embedded-font notices
from the verified Cargo registry packages. Unused optional dependencies are not
part of the distributed executable. No third-party executable code is vendored here.

The enum-map LICENSES/MIT.txt and README copyright headers come from the exact upstream commit mirrored on GitHub. dispatch 0.2.0 declares MIT in its original Cargo.toml but provides no license file; the same author Steven Sheldon's MIT text from his objc project is included alongside that declaration. This exception and both pinned source URLs are recorded in licenses.json and the distributed notices.

hexf-parse declares CC0-1.0 in its original Cargo.toml without including the legal text. That exact declaration and the canonical SPDX CC0-1.0 text (versioned source and SHA-256) are supplied together.
