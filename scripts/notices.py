"""Collect bundled dependency license texts for the Windows distribution."""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path

def fail(message):
    annotation = message.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    print("::error::" + annotation)
    raise SystemExit(message)

parser = argparse.ArgumentParser()
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
vendor_root = Path(__file__).resolve().parent.parent/"third_party"
vendor = {(v["package"], v["version"]):v for v in json.loads((vendor_root/"licenses.json").read_text())}
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--locked", "--offline", "--format-version", "1",
    "--filter-platform", "x86_64-pc-windows-msvc",
], text=True, encoding="utf-8"))
packages = {p["id"]: p for p in metadata["packages"]}
tree = subprocess.check_output([
    "cargo", "tree", "--workspace", "--locked", "--offline",
    "--target", "x86_64-pc-windows-msvc", "--edges", "normal,build",
    "--prefix", "none", "--format", "{p}",
], text=True, encoding="utf-8")
selected = {(line.split()[0], line.split()[1].removeprefix("v"))
            for line in tree.splitlines() if len(line.split()) >= 2}
visited = {p["id"] for p in metadata["packages"] if (p["name"], p["version"]) in selected}
sections = ["Rescue third-party notices\n\nPackage license expressions and supplied license texts follow.\n"]
missing = []
for package_id in sorted(visited):
    package = packages[package_id]
    if package["source"] is None:
        continue
    root = Path(package["manifest_path"]).parent
    paths = set()
    if package.get("license_file"):
        paths.add(root / package["license_file"])
    for path in root.rglob("*"):
        if path.is_file() and len(path.relative_to(root).parts) <= 3:
            upper = path.name.upper()
            if (upper.startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"))
                or "LICENSE" in upper or upper in ("OFL.TXT", "UFL.TXT")
                or (package["name"] == "epaint_default_fonts" and path.suffix == ".txt")):
                paths.add(path)
    text = [f'\n{"="*72}\n{package["name"]} {package["version"]}\nLicense: {package.get("license")}\n']
    found = False
    for item in vendor.get((package["name"], package["version"]), {}).get("files", []):
        content = (vendor_root/item["path"]).read_bytes()
        if hashlib.sha256(content).hexdigest() != item["sha256"]:
            fail("Vendor license checksum mismatch: " + item["path"])
        text.append(f'\n--- Upstream: {item["url"]} ---\n{content.decode("utf-8")}\n')
        found = True
    for path in sorted(paths):
        if path.is_file() and path.stat().st_size <= 1024*1024:
            try:
                content = path.read_text(encoding="utf-8")
            except UnicodeError:
                continue
            text.append(f"\n--- {path.relative_to(root)} ---\n{content}\n")
            found = True
    if not found:
        missing.append(package["name"])
    sections.append("".join(text))
if missing:
    fail("License texts missing; review before distribution: " + ", ".join(missing))
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text("\n".join(sections), encoding="utf-8")
print(f"Collected notices for {len(sections)-1} dependencies")
