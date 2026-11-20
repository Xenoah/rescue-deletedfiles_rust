"""Verify native volume-relative extent maps and location-independent chunk fingerprints on deleted camera RAW data."""

import argparse
import gzip
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from e2e import check, run
from verify_camera_raw_metadata import digest, equal

FIXTURES = Path(__file__).resolve().parents[1] / "tests/camera-raw"
CHUNK = 4096


def refusal(binary, *args, output):
    result = subprocess.run([binary, *args, "--output", output, "--json"], capture_output=True, timeout=300)
    check(result.returncode != 0 and not output.exists(), (args, result.stdout))
    err = json.loads(result.stdout or result.stderr)
    return err["error"]["code"]


def public_metadata_paths(binary, disk, kind, sources, folder):
    strict = run(binary, "scan", "--source", disk, "--work-dir", folder / "strict", "--json")
    rows = run(binary, "results", "--session", strict["session"], "--json")["candidates"]
    listed = run(binary, "filesystem", "--source", disk, "--fs-type", kind, "--json")["candidates"]
    info = dict(
        current_metadata_listed=len(rows),
        current_metadata_supported=sum(r["supported"] for r in rows),
        sleuthkit_listed=len(listed),
    )
    if kind == "ntfs":
        check(not rows and not listed, info)
        return info
    check(len(rows) == len(listed) == len(sources) and not any(r["supported"] for r in rows), info)
    output = folder / "outputs"
    estimate = run(
        binary,
        "scan",
        "--source",
        disk,
        "--work-dir",
        folder / "estimate",
        "--estimate-fat-contiguous",
        "--json",
    )
    recovered = run(
        binary, "recover", "--session", estimate["session"], "--all", "--destination", output, "--json"
    )["results"]
    hashes = {m["sha256"] for m in sources}
    suffixes = {Path(m["name"]).suffix for m in sources}

    def inspect_outputs():
        saved = []
        for file in sorted(output.rglob("*")):
            if file.is_file() and file.suffix.lower() in suffixes:
                h = digest(file)
                saved.append(
                    dict(
                        name=file.name,
                        bytes=file.stat().st_size,
                        output_sha256=h,
                        matched_any_original=h in hashes,
                    )
                )
        check(len(saved) == len(sources) and not any(x["matched_any_original"] for x in saved), saved)
        return sorted(saved, key=lambda x: x["name"])

    evidence = sorted({item["extent_evidence"] for item in recovered})
    check(evidence == ["estimated_contiguous"], evidence)
    estimated = inspect_outputs()
    shutil.rmtree(output)
    run(
        binary,
        "filesystem",
        "--source",
        disk,
        "--fs-type",
        "fat32",
        "--all",
        "--destination",
        output,
        "--json",
        timeout=300,
    )
    info["estimated_extent_evidence"] = evidence
    info["unverified_fat_outputs"] = dict(estimated_outputs=estimated, sleuthkit_outputs=inspect_outputs())
    return info


def verify(binary, originals, images, layouts, folder):
    sources = json.loads((FIXTURES / "sources.json").read_text())
    volumes = json.loads(
        gzip.decompress(layouts.read_bytes()) if layouts.suffix == ".gz" else layouts.read_bytes()
    )
    fingerprints = {}
    for meta in sources:
        name = meta["name"]
        source = originals / name
        check(source.stat().st_size == meta["bytes"] and digest(source) == meta["sha256"], source)
        path = folder / (name + ".fp.json")
        row = run(
            binary,
            "fingerprint-file",
            "--source",
            source,
            "--chunk-size",
            str(CHUNK),
            "--output",
            path,
            "--json",
        )
        check(row["sha256"] == meta["sha256"] and row["chunks"] == (meta["bytes"] + CHUNK - 1) // CHUNK, row)
        fingerprints[name] = path
    output = {}
    for kind, volume in volumes.items():
        disk = images / (kind + ".raw")
        before = digest(disk)
        check(before == volume["source_sha256"], disk)
        records = {}
        for meta in sources:
            name = meta["name"]
            layout = volume["files"][name]
            check(layout["sha256"] == meta["sha256"], name)
            extents = layout["extents"]
            check(len(extents) > 1 and sum(e["length"] for e in extents) == meta["bytes"], name)
            check(all(a["offset"] + a["length"] != b["offset"] for a, b in zip(extents, extents[1:])), name)
            source = originals / name
            plan = folder / "active.json"
            plan.write_text(json.dumps(layout))
            saved = folder / "restored.raw"
            by_map = run(
                binary, "recover-extents", "--source", disk, "--layout", plan, "--output", saved, "--json"
            )
            check(
                by_map["original_sha256_verified"]
                and by_map["file"]["sha256"] == digest(saved) == meta["sha256"]
                and equal(saved, source),
                name,
            )
            saved.unlink()
            by_hash = run(
                binary,
                "recover-fingerprinted",
                "--source",
                disk,
                "--manifest",
                fingerprints[name],
                "--alignment",
                "512",
                "--output",
                saved,
                "--json",
                timeout=300,
            )
            check(
                by_hash["original_sha256_verified"]
                and by_hash["file"]["sha256"] == digest(saved) == meta["sha256"]
                and equal(saved, source),
                name,
            )
            check(by_hash["matched_chunks"] == (meta["bytes"] + CHUNK - 1) // CHUNK, by_hash)
            saved.unlink()
            records[name] = dict(
                bytes=meta["bytes"],
                sha256=meta["sha256"],
                runs=len(extents),
                fingerprint_chunks=by_hash["matched_chunks"],
                distinct_chunks=by_hash["distinct_chunks"],
                both_outputs_byte_exact=True,
            )
        name = "camera.dng"
        plan = folder / "incorrect.json"
        bad = folder / "must-not-exist.raw"
        corrected = volume["files"][name]
        swapped = dict(corrected, extents=list(reversed(corrected["extents"])))
        plan.write_text(json.dumps(swapped))
        controls = {
            "reversed_layout": refusal(
                binary, "recover-extents", "--source", disk, "--layout", plan, output=bad
            )
        }
        other = "fat32" if kind == "ntfs" else "ntfs"
        plan.write_text(json.dumps(volumes[other]["files"][name]))
        controls["other_volume_layout"] = refusal(
            binary, "recover-extents", "--source", disk, "--layout", plan, output=bad
        )
        plan.write_text(json.dumps(dict(corrected, sha256="0" * 64)))
        controls["wrong_original_sha256"] = refusal(
            binary, "recover-extents", "--source", disk, "--layout", plan, output=bad
        )
        finger = json.loads(fingerprints[name].read_text())
        finger["chunks"][0]["sha256"] = "0" * 64
        plan.write_text(json.dumps(finger))
        controls["wrong_chunk_sha256"] = refusal(
            binary,
            "recover-fingerprinted",
            "--source",
            disk,
            "--manifest",
            plan,
            "--alignment",
            "512",
            output=bad,
        )
        check(digest(disk) == before, disk)
        output[kind] = dict(
            source_sha256=before,
            map_api=volume["map_api"],
            files=records,
            controls_without_output=controls,
            total_runs=sum(v["runs"] for v in records.values()),
        )
        with tempfile.TemporaryDirectory(dir=folder) as scratch:
            output[kind].update(public_metadata_paths(binary, disk, kind, sources, Path(scratch)))
        check(digest(disk) == before, disk)
        print(kind, len(records), "fragmented originals byte- and SHA-256-exact by both methods", flush=True)
    return {
        "version": subprocess.check_output([binary, "--version"], text=True).strip(),
        "binary_sha256": digest(binary),
        "chunk_bytes": CHUNK,
        "search_alignment_bytes": 512,
        "sources_unchanged": True,
        "volumes": output,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "originals", "images", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--layouts", type=Path, default=FIXTURES / "kernel-fragments/layouts.json.gz")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        report = verify(args.binary.resolve(), args.originals, args.images, args.layouts, Path(tmp))
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
