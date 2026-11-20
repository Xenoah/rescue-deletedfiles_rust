"""Exercise partition-relative layouts with GNU Parted and the two Linux camera-card captures."""

import argparse
import gzip
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from e2e import check, run
from verify_camera_raw_metadata import digest, equal
from verify_fragmented_cameras import FIXTURES

sys.path.insert(0, str(FIXTURES / "kernel-fragments"))
from pack_layouts import parse_map


def partition_image(image, card, scheme):
    sectors = card.stat().st_size // 512
    start = 4096 if scheme == "logical" else 2048
    end = start + sectors - 1
    with image.open("xb") as f:
        f.truncate((end + 4097) * 512)

    def parted(*args):
        return subprocess.run(
            ["parted", "--script", "--align", "none", image, *args],
            check=True,
            capture_output=True,
            text=True,
        )

    if scheme in ("mbr", "logical"):
        parted("mklabel", "msdos")
        if scheme == "logical":
            parted("unit", "s", "mkpart", "extended", "2048", str(end + 2048))
        parted(
            "unit",
            "s",
            "mkpart",
            "logical" if scheme == "logical" else "primary",
            "ntfs",
            str(start),
            str(end),
        )
    else:
        parted("mklabel", scheme)
        parted("unit", "s", "mkpart", "Rescue", "ntfs", str(start), str(end))
    index = {"gpt": 1, "mbr": 1, "mac": 2, "logical": 5}[scheme]
    table = parted("--machine", "unit", "B", "print").stdout.splitlines()
    (row,) = [r.split(":") for r in table if r.startswith(str(index) + ":")]
    check(row[1] == str(start * 512) + "B" and row[3] == str(card.stat().st_size) + "B", row)
    with image.open("r+b") as dest, card.open("rb") as src:
        dest.seek(start * 512)
        shutil.copyfileobj(src, dest)
    return index, start * 512


def verify(binary, camera_originals, cards, exfat, exfat_guest):
    sources = json.loads((FIXTURES / "sources.json").read_text())
    captured = json.loads(gzip.decompress((FIXTURES / "kernel-fragments/layouts.json.gz").read_bytes()))
    guest = exfat_guest.read_text()
    report = {}
    for case, scheme in [("ntfs", "gpt"), ("fat32", "logical"), ("contiguous", "mac"), ("fragmented", "mbr")]:
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            if case in captured:
                card = cards / (case + ".raw")
                volume = captured[case]
            else:
                card = exfat / (case + ".raw")
                records = guest.split("FRAG_START_" + case + "\n")[-1].split("FRAG_END_" + case + "\n")[0]
                path = folder / "actual.map"
                path.write_text(
                    "\n".join(
                        line for line in records.splitlines() if line.startswith(("FILE ", "RUN ", "END "))
                    )
                    + "\n"
                )
                volume = parse_map(path, camera_originals, card, require_fragmented=case == "fragmented")
                device = "vdc" if case == "fragmented" else "vdb"
                check(volume["source_sha256"] + "  /dev/" + device in guest.splitlines(), case)
            check(digest(card) == volume["source_sha256"], card)
            image = folder / "disk.raw"
            index, offset = partition_image(image, card, scheme)
            before = digest(image)
            table = run(binary, "inspect", "--source", image, "--json")["layout"]
            (region,) = [r for r in table["regions"] if r["index"] == index]
            check(region["offset"] == offset and region["length"] == card.stat().st_size, region)
            plan = folder / "file.json"
            saved = folder / "saved.raw"
            files = {}
            for meta in sources:
                name = meta["name"]
                layout = volume["files"][name]
                check(layout["sha256"] == meta["sha256"] == digest(camera_originals / name), name)
                plan.write_text(json.dumps(dict(layout, schema_version=2, partition=index)))
                r = run(
                    binary,
                    "recover-extents",
                    "--source",
                    image,
                    "--layout",
                    plan,
                    "--output",
                    saved,
                    "--json",
                )
                check(
                    r["original_sha256_verified"]
                    and r["file"]["sha256"] == digest(saved) == meta["sha256"]
                    and equal(saved, camera_originals / name),
                    name,
                )
                saved.unlink()
                files[name] = dict(bytes=meta["bytes"], sha256=meta["sha256"], runs=len(layout["extents"]))
            bad = dict(volume["files"]["camera.dng"], schema_version=2, partition=index, sha256="0" * 64)
            plan.write_text(json.dumps(bad))
            run(
                binary,
                "recover-extents",
                "--source",
                image,
                "--layout",
                plan,
                "--output",
                saved,
                "--json",
                expected=4,
            )
            check(
                not saved.exists() and digest(image) == before and digest(card) == volume["source_sha256"],
                case,
            )
            report[case] = dict(
                scheme=table["scheme"],
                partition=index,
                offset=offset,
                disk_sha256=before,
                source_volume_sha256=volume["source_sha256"],
                files=files,
                wrong_digest_refused=True,
            )
            print(case, scheme, index, len(files), "originals restored", flush=True)
    return dict(
        tool=subprocess.check_output(["parted", "--version"], text=True).splitlines()[0],
        binary_sha256=digest(binary),
        volumes=report,
    )


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    for arg in ("binary", "originals", "cards", "exfat", "exfat-guest", "output"):
        p.add_argument("--" + arg, type=Path, required=True)
    a = p.parse_args()
    report = verify(a.binary.resolve(), a.originals, a.cards, a.exfat, a.exfat_guest)
    a.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
