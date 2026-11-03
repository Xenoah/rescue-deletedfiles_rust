"""Package validated Windows binaries; never generates or claims test evidence."""
import argparse
import hashlib
import tomllib
import zipfile
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--validate-tag")
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
version = tomllib.loads((root/"Cargo.toml").read_text())["workspace"]["package"]["version"]
if args.validate_tag is not None:
    if args.validate_tag != f"v{version}":
        raise SystemExit(f"Tag {args.validate_tag} does not match workspace {version}")
    raise SystemExit(0)
files = {
    "rescue.exe": root/"target/release/rescue.exe",
    "rescue-gui.exe": root/"target/release/rescue-gui.exe",
    "README.md": root/"README.md",
    "LICENSE": root/"LICENSE",
    "THIRD_PARTY_NOTICES.txt": root/"dist/THIRD_PARTY_NOTICES.txt",
    "docs/ALPHA.md": root/"docs/ALPHA.md",
    "docs/SPEC.md": root/"docs/SPEC.md",
}
for name, path in files.items():
    if not path.is_file():
        raise SystemExit(f"Required distribution file missing: {name}")
dist = root/"dist"
dist.mkdir(exist_ok=True)
archive = dist/f"rescue-{version}-windows-x64.zip"
with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED) as z:
    for name, path in files.items():
        z.write(path, name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(dist/"SHA256SUMS.txt").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
print(archive)
