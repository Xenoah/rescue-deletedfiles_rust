"""Package validated Windows binaries; never generates or claims test evidence."""
import argparse
import base64
import shutil
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
    "docs/GOAL.md": root/"docs/GOAL.md",
    "docs/VALIDATION.md": root/"docs/VALIDATION.md",
    "docs/intelligence.html": root/"docs/intelligence.html",
    "docs/images/intelligence.png": root/"docs/images/intelligence.png",
    "docs/images/technical-ui.png": root/"docs/images/technical-ui.png",
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
# The release HTML is self-contained; the ZIP retains the documented relative paths.
html = (root/"docs/intelligence.html").read_text(encoding="utf-8")
encoded = base64.b64encode((root/"docs/images/intelligence.png").read_bytes()).decode("ascii")
html = html.replace('src="images/intelligence.png"', f'src="data:image/png;base64,{encoded}"')
(dist/"intelligence.html").write_text(html, encoding="utf-8")
for name in ("intelligence.png", "technical-ui.png"):
    shutil.copyfile(root/"docs/images"/name, dist/name)
assets = [archive, dist/"intelligence.html", dist/"intelligence.png", dist/"technical-ui.png"]
checksums = "".join(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n" for path in assets)
(dist/"SHA256SUMS.txt").write_text(checksums, encoding="ascii")
print(archive)
