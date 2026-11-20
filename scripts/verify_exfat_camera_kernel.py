"""Verify actual Linux exFAT unlinks against kernel-recorded maps and independent camera originals."""

import argparse
import gzip
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

from e2e import check, run
from verify_camera_raw_metadata import digest, equal

FIXTURES = Path(__file__).resolve().parents[1] / "tests/camera-raw"
sys.path.insert(0, str(FIXTURES / "kernel-fragments"))
from pack_layouts import parse_map


def listing(binary, image, work):
    scan = run(binary, "scan", "--source", image, "--work-dir", work, "--json")
    rows = run(binary, "results", "--session", scan["session"], "--json")["candidates"]
    return scan["session"], {entry["name"]: entry for entry in rows}


def damage_controls(binary, image, folder, originals, layouts, fragmented):
    name = "camera.dng"
    block = layouts[name]["extents"][0]["offset"]
    with image.open("rb") as f:
        header = f.read(512)
        sector = 1 << header[108]
        cluster_size = sector << header[109]
        fat = struct.unpack_from("<I", header, 80)[0] * sector
        heap = struct.unpack_from("<I", header, 88)[0] * sector
        root = struct.unpack_from("<I", header, 96)[0]
        f.seek(heap + (root - 2) * cluster_size)
        entries = f.read(cluster_size)
    bitmap = [
        struct.unpack_from("<I", entries, i + 20)[0] for i in range(0, cluster_size, 32) if entries[i] == 0x81
    ]
    check(len(bitmap) == 1 and (block - heap) % cluster_size == 0, name)
    cluster = 2 + (block - heap) // cluster_size
    # The captured disks use one bitmap whose first cluster contains this bit.
    check((cluster - 2) // 8 < cluster_size, cluster)
    bit_offset = heap + (bitmap[0] - 2) * cluster_size + (cluster - 2) // 8
    controls = {}
    for kind in ["allocation_bit_set", "data_byte_changed"] + (
        ["cleared_chain", "self_linked_chain"] if fragmented else []
    ):
        with tempfile.TemporaryDirectory(dir=folder) as scratch:
            root = Path(scratch)
            copy = root / "changed.raw"
            shutil.copyfile(image, copy)
            with copy.open("r+b") as f:
                if kind == "allocation_bit_set":
                    f.seek(bit_offset)
                    old = f.read(1)[0]
                    bit = 1 << ((cluster - 2) % 8)
                    check(not old & bit, old)
                    f.seek(bit_offset)
                    f.write(bytes([old | bit]))
                elif kind == "data_byte_changed":
                    f.seek(block + 17)
                    old = f.read(1)[0]
                    f.seek(block + 17)
                    f.write(bytes([old ^ 1]))
                else:
                    f.seek(fat + cluster * 4)
                    f.write(struct.pack("<I", 0 if kind == "cleared_chain" else cluster))
            session, rows = listing(binary, copy, root / "session")
            target = rows[name]
            check(target["supported"] == (kind == "data_byte_changed"), (kind, target))
            dest = root / "export"
            proc = subprocess.run(
                [
                    binary,
                    "recover",
                    "--session",
                    session,
                    "--id",
                    target["id"],
                    "--destination",
                    dest,
                    "--json",
                ],
                capture_output=True,
                timeout=120,
            )
            saved = list(dest.rglob(name))
            if kind == "data_byte_changed":
                check(proc.returncode == 0 and len(saved) == 1, proc.stdout)
                check(
                    not equal(saved[0], originals / name) and digest(saved[0]) != layouts[name]["sha256"],
                    kind,
                )
                controls[kind] = dict(
                    metadata_supported=True, observed_sha256=digest(saved[0]), matches_original=False
                )
            else:
                check(proc.returncode != 0 and not saved, (kind, proc.stdout))
                controls[kind] = dict(
                    metadata_supported=False, file_written=False, warnings=target["warnings"]
                )
    return controls


def verify(binary, originals, images, guest_log):
    sources = {row["name"]: row for row in json.loads((FIXTURES / "sources.json").read_text())}
    for name, entry in sources.items():
        check(
            digest(originals / name) == entry["sha256"]
            and (originals / name).stat().st_size == entry["bytes"],
            name,
        )
    log = (
        gzip.decompress(guest_log.read_bytes()).decode()
        if guest_log.suffix == ".gz"
        else guest_log.read_text()
    )
    check("CAMERA_KERNEL_OK" in log.splitlines(), "missing completed guest capture")
    result = {}
    for case, device in [("contiguous", "vdb"), ("fragmented", "vdc")]:
        image = images / (case + ".raw")
        before = digest(image)
        check(
            f"{before}  /dev/{device}" in log.splitlines()
            or f"VOLUME_SHA256 {case} {before}" in log.splitlines(),
            "Guest/host volume mismatch",
        )
        # Only accept complete captures taken before unlink, never output from the recovery program.
        segment = log.split("FRAG_START_" + case + "\n")[-1].split("FRAG_END_" + case + "\n")[0]
        with tempfile.TemporaryDirectory(dir=images) as scratch:
            folder = Path(scratch)
            capture = folder / "actual.map"
            capture.write_text(
                "\n".join(line for line in segment.splitlines() if line.startswith(("FILE ", "RUN ", "END ")))
                + "\n"
            )
            volumes = parse_map(capture, originals, image, require_fragmented=case == "fragmented")
            layouts = volumes["files"]
            counts = {n: len(m["extents"]) for n, m in layouts.items()}
            check(volumes["source_sha256"] == before and set(layouts) == set(sources), case)
            if case == "contiguous":
                check(set(counts.values()) == {1}, counts)
            evidence = "residual_fat_chain" if case == "fragmented" else "no_fat_chain"
            session, rows = listing(binary, image, folder / "session")
            check(
                set(rows) == set(sources)
                and all(r["supported"] and r["extent_evidence"] == evidence for r in rows.values()),
                case,
            )
            files = {}
            dest = folder / "exports"
            native = run(
                binary, "recover", "--session", session, "--all", "--destination", dest, "--json", timeout=300
            )["results"]
            check(len(native) == len(sources), case)
            for row in native:
                name = row["naming"]["original_name"]
                path = Path(row["path"])
                meta = sources[name]
                check(
                    name not in files
                    and row["status"] == "completed"
                    and row["extent_evidence"] == evidence
                    and row["sha256"] == digest(path) == meta["sha256"]
                    and equal(path, originals / name),
                    name,
                )
                files[name] = dict(
                    bytes=meta["bytes"], sha256=meta["sha256"], runs=counts[name], native_byte_exact=True
                )
            shutil.rmtree(dest)
            external = run(
                binary,
                "filesystem",
                "--source",
                image,
                "--fs-type",
                "exfat",
                "--all",
                "--destination",
                dest,
                "--json",
                timeout=300,
            )["results"]
            seen = set()
            for row in external:
                name = Path(row["original_path"]).name
                saved = Path(row["path"])
                entry = files[name]
                check(
                    name not in seen
                    and saved.stat().st_size == entry["bytes"]
                    and digest(saved) == row["sha256"],
                    row,
                )
                seen.add(name)
                exact = equal(saved, originals / name)
                check(exact == (entry["sha256"] == row["sha256"]), row)
                entry.update(sleuthkit_output_sha256=row["sha256"], sleuthkit_byte_exact=exact)
            check(seen == set(sources), seen)
            shutil.rmtree(dest)
            controls = damage_controls(binary, image, folder, originals, layouts, case == "fragmented")
        check(digest(image) == before, image)
        result[case] = dict(
            source_sha256=before,
            map_api=volumes["map_api"],
            extent_evidence=evidence,
            files=files,
            controls=controls,
        )
        print(
            case,
            "native",
            len(files),
            "Sleuth Kit",
            sum(f["sleuthkit_byte_exact"] for f in files.values()),
            "exact",
            flush=True,
        )
    return dict(
        version=subprocess.check_output([binary, "--version"], text=True).strip(),
        binary_sha256=digest(binary),
        sleuthkit=subprocess.check_output(["fls", "-V"], text=True).strip(),
        actual_unlinks=32,
        sources_unchanged=True,
        volumes=result,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("binary", "originals", "images", "guest-log", "output"):
        parser.add_argument("--" + arg, type=Path, required=True)
    args = parser.parse_args()
    report = verify(args.binary.resolve(), args.originals.resolve(), args.images.resolve(), args.guest_log)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
