"""Pack live kernel extent records after independently checking every range in each unmounted disk image."""

import argparse
import gzip
import hashlib
import json
from pathlib import Path


def parse_map(path, originals, disk, *, require_fragmented=True):
    names = {p.name for p in originals.glob("camera.*") if p.is_file()}
    files = {}
    apis = set()
    name = None
    with disk.open("rb") as image:
        source_sha256 = hashlib.file_digest(image, "sha256").hexdigest()
        for line in path.read_text().splitlines():
            words = line.split()
            if not words:
                continue
            if words[0] == "FILE" and len(words) == 3 and name is None:
                name = Path(words[1]).name
                if name not in names or name in files:
                    raise ValueError(f"Unexpected or repeated name {name}")
                source = originals / name
                length = int(words[2])
                if length != source.stat().st_size:
                    raise ValueError(f"Incorrect length for {name}")
                position = 0
                extents = []
            elif words[0] == "RUN" and len(words) == 4 and name is not None:
                logical, offset, size = map(int, words[1:])
                if logical != position or offset < 0 or size <= 0 or position + size > length:
                    raise ValueError(f"Invalid logical or physical range for {name}")
                extents.append({"offset": offset, "length": size})
                position += size
            elif words[0] == "END" and words[1:] in (["FIEMAP"], ["FIBMAP"]) and name is not None:
                if position != length or not extents or (require_fragmented and len(extents) < 2):
                    raise ValueError(f"Incomplete or unfragmented camera file {name}")
                h = hashlib.sha256()
                with source.open("rb") as truth:
                    for extent in extents:
                        image.seek(extent["offset"])
                        remaining = extent["length"]
                        while remaining:
                            count = min(remaining, 1 << 20)
                            chunk = image.read(count)
                            if len(chunk) != count or chunk != truth.read(count):
                                raise ValueError(f"Disk range differs from {name}")
                            h.update(chunk)
                            remaining -= count
                files[name] = {"schema_version": 1, "sha256": h.hexdigest(), "extents": extents}
                apis.add(words[1])
                name = None
            else:
                raise ValueError(f"Unexpected capture record {line}")
    if name is not None or files.keys() != names:
        raise ValueError("Incomplete set of camera files")
    return {"source_sha256": source_sha256, "map_api": sorted(apis), "files": files}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--originals", required=True, type=Path)
    parser.add_argument(
        "--images", required=True, type=Path, help="Directory containing ntfs.raw and fat32.raw"
    )
    parser.add_argument(
        "--logs", required=True, type=Path, help="Directory containing ntfs.map and fat32.map"
    )
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    cases = {}
    for fs, driver in [("ntfs", "ntfs3"), ("fat32", "vfat")]:
        cases[fs] = parse_map(args.logs / (fs + ".map"), args.originals, args.images / (fs + ".raw"))
        cases[fs]["driver"] = driver
    with args.output.open("xb") as out:
        out.write(gzip.compress((json.dumps(cases, separators=(",", ":")) + "\n").encode(), mtime=0))
