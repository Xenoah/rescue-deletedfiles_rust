"""Reconstruct Linux persistent dm-snapshot histories and verify deleted, live and overwritten views."""
import argparse
import json
import lzma
import re
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_lvm2 import FILES, apply_text, sha, source_args

GOLD = FILES.parents[1] / "lvm2-snapshots"


def carved(binary, disk, work, truth, allocated, indexed=False):
    scan = run(binary, "scan", "--source", disk, "--mode", "carve", "--carve-scope",
               "all" if allocated else "unallocated", *(["--indexed"] if indexed else []), "--work-dir", work, "--json")
    rows = run(binary, "results", "--session", scan["session"], "--json")["candidates"]
    names = {h: n for n, h in truth.items()}
    ids = {row["record_sha256"]: row["id"] for row in rows if row.get("record_sha256") in names}
    check(ids.keys() == names.keys(), (work, ids.keys(), names.keys()))
    chosen = [v for ident in ids.values() for v in ("--id", ident)]
    result = run(binary, "recover", "--session", scan["session"], *chosen, "--destination", work / "out", "--json")
    check(len(result["results"]) == len(names), result)
    for row in result["results"]:
        data = Path(row["path"]).read_bytes()
        check(data == (FILES / names[row["sha256"]]).read_bytes() and sha(data) == row["sha256"], row)


def sleuthkit(disk, files, previous=None):
    live = subprocess.check_output(["fls", "-uFrp", "-f", "ext4", disk], text=True)
    deleted = subprocess.check_output(["fls", "-dFrp", "-f", "ext4", disk], text=True) if previous else ""
    numbers = {}
    for name, h in files.items():
        matches = [line for line in live.splitlines() if line.endswith("\t" + name)]
        if previous:
            check(not matches, (disk, name, matches))
            ino = previous[name]
            check(re.search(rf"\* {ino}:\t", deleted), (disk, ino))
            stat = subprocess.check_output(["istat", "-f", "ext4", disk, str(ino)], text=True)
            check("Not Allocated" in stat and "num of links: 0" in stat, (name, stat))
        else:
            check(len(matches) == 1, (disk, name, matches))
            inode = re.search(r" (\d+):\t", matches[0])
            check(inode, matches[0])
            ino = int(inode[1])
            data = subprocess.check_output(["icat", "-f", "ext4", disk, str(ino)])
            check(data == (FILES / name).read_bytes() and sha(data) == h, name)
        numbers[name] = ino
    return numbers


def verify_layout(binary, root, folder, native):
    root.mkdir()
    proof = json.loads((folder / "manifest.json").read_text(encoding="utf-8"))
    pvs = []
    for name, stored in sorted(proof["physical"].items()):
        raw = lzma.decompress((folder / (name + ".raw.xz")).read_bytes())
        p = root / (name + ".raw")
        p.write_bytes(raw)
        check(len(raw) == stored["bytes"] and sha(raw) == stored["sha256"], name)
        pvs.append(p)
    listing = run(binary, "lvm-volumes", *source_args(pvs), "--json")["volumes"]
    check(run(binary, "lvm-volumes", *source_args(pvs[::-1]), "--json")["volumes"] == listing, listing)
    views = {r["name"]: r for r in listing}
    expected_names = {("data" if n == "active" else n) for n in proof["logical"]} | set(proof["kernel_refused"])
    check(views.keys() == expected_names and all(r["group"] == "history" for r in listing), listing)
    if proof["lvs"]:
        for n, identity in proof["lvs"].items():
            check(views[n]["id"] == identity["id"], (n, identity))
            if n != "data":
                s = views[n]["snapshot"]
                check(s["origin_uuid"] == identity["origin_id"] and s["cow_bytes"] == identity["lvm_reported_bytes"], s)
    results = {}
    for n in ["before", "deleted", "active", *(["fresh"] if "fresh" in proof["logical"] else [])]:
        truth = proof["logical"][n]
        name = "data" if n == "active" else n
        view = views[name]
        wanted = lzma.decompress((folder / (n + ".raw.xz")).read_bytes())
        check(sha(wanted) == truth["volume_sha256"] and view["bytes"] == len(wanted), name)
        if n != "active":
            check(view["snapshot"]["origin"] == "data" and view["snapshot"]["origin_uuid"] == views["data"]["id"], view)
        for label, selector, paths in [("ordinary", name, pvs), ("index", view["id"], pvs[::-1])]:
            disk = root / f"{n}-{label}.img"
            info = run(binary, "import-image", "--image-type", "lvm2", *source_args(paths), "--lvm-volume", selector,
                       "--raw-bytes", str(len(wanted)), "--raw-sha256", truth["volume_sha256"], "--output", disk, "--json")
            check(disk.read_bytes() == wanted and sha(disk.read_bytes()) == info["sha256"] == truth["volume_sha256"], info)
            carved(binary, disk, root / (label + "-" + n), truth["matched"], n != "deleted", label == "index")
            if native and label == "ordinary" and n in ("before", "deleted"):
                results["ext4_" + n] = sleuthkit(disk, truth["matched"], results.get("ext4_before") if n == "deleted" else None)
            disk.unlink()
        results[n] = {"metadata": view, **truth}
    for name in proof["kernel_refused"]:
        p = root / (name + "-unavailable.img")
        reply = subprocess.run([binary, "import-image", "--image-type", "lvm2", *source_args(pvs), "--lvm-volume", name,
                                "--output", p, "--json"], capture_output=True)
        check(reply.returncode != 0 and not p.exists(), name)
    results["kernel_refused"] = proof["kernel_refused"]
    for p, stored in zip(pvs, proof["physical"].values()):
        check(sha(p.read_bytes()) == stored["sha256"], p)
    print(folder.name, len(proof["logical"]), "full kernel volumes and", len(proof["logical"]["deleted"]["matched"]),
          "actual ext4 unlinks exact; current bytes checked separately", flush=True)
    return results


def damaged(binary, root):
    base = root / "striped-origin"
    a, b = (base / "pv-a.raw"), (base / "pv-b.raw")
    src = a.read_bytes()
    toc = json.loads((GOLD / "striped-origin/manifest.json").read_text(encoding="utf-8"))
    # The kernel places this store at sector 18432 on the first PV.
    check(any(re.fullmatch(r"history-before-cow: 0 \d+ linear \d+:0 18432", l) for l in toc["table"]), toc["table"])
    origin = 18432 * 512
    chunk = 8192
    first = origin + chunk
    check(src[origin:origin + 4] == b"SnAp" and struct.unpack_from("<QQ", src, first) == (0, 2), src[origin:origin+16])
    modified = root / "changed.pv"
    refused = []
    def reject(label, members):
        out = root / (label + ".raw")
        args = [binary, "import-image", "--image-type", "lvm2", *source_args(members), "--lvm-volume", "before",
                "--output", out, "--json"]
        p = subprocess.run(args, capture_output=True)
        check(p.returncode != 0 and not out.exists(), (label, p.stdout))
        refused.append(label)
    cases = [
        ("bad-header-magic", origin, "<I", 0), ("invalidated-store", origin+4, "<I", 0),
        ("unknown-store-version", origin+8, "<I", 2), ("chunk-size-mismatch", origin+12, "<I", 128),
        ("origin-outside", first, "<Q", 2048), ("store-outside", first+8, "<Q", 99999),
        ("duplicate-origin", first+16, "<Q", 0), ("duplicate-store", first+24, "<Q", 2),
        ("data-refers-to-header-page", first+8, "<Q", 1), ("data-refers-to-second-metadata-page", first+8, "<Q", 514),
        ("hole-before-committed-exceptions", first+8, "<Q", 0), ("old-without-new", first+24, "<Q", 0),
        ("third-page-damage", origin+1027*chunk+8, "<Q", 1),
    ]
    for label, offset, fmt, value in cases:
        data = bytearray(src)
        struct.pack_into(fmt, data, offset, value)
        modified.write_bytes(data)
        reject(label, [modified, b])
    # One damaged historical store cannot change its origin or an unrelated healthy snapshot.
    siblings = {}
    for lv, oracle in [("data", "active"), ("deleted", "deleted")]:
        dest = root / ("unaffected-" + lv + ".img")
        expected = toc["logical"][oracle]["volume_sha256"]
        run(binary, "import-image", *source_args([modified, b]), "--lvm-volume", lv,
            "--output", dest, "--raw-sha256", expected, "--json")
        check(dest.read_bytes() == lzma.decompress((GOLD / "striped-origin" / (oracle + ".raw.xz")).read_bytes()), lv)
        siblings[lv] = sha(dest.read_bytes())
    other = root / "changed-b.pv"
    for label, old, new in [
        ("missing-origin", b'origin = "data"', b'origin = "absent"'),
        ("missing-store", b'cow_store = "before"', b'cow_store = "absent"'),
        ("overlapping-stores", b'cow_store = "deleted"', b'cow_store = "before"'),
        ("cyclic-dependency", b'origin = "data"', b'origin = "before"'),
        ("unsupported-merge", b'type = "snapshot"', b'type = "snapshot-merge"'),
        ("invalid-chunk", b"chunk_size = 16", b"chunk_size = 3"),
        ("hidden-origin", b'origin = "data"', b'origin = "deleted"'),
    ]:
        for source, dest in [(a, modified), (b, other)]:
            data = bytearray(source.read_bytes())
            apply_text(data, lambda doc: doc.replace(old, new, 1))
            dest.write_bytes(data)
        reject(label, [modified, other])
    data = bytearray(src)
    data[origin+2*chunk+301] ^= 1
    modified.write_bytes(data)
    dest = root / "changed-valid.img"
    args = [binary, "import-image", *source_args([modified,b]), "--lvm-volume", "before", "--output", dest, "--json"]
    fail = subprocess.run([*args, "--raw-sha256", toc["logical"]["before"]["volume_sha256"]], capture_output=True)
    check(fail.returncode != 0 and not dest.exists(), fail)
    refused.append("valid-data-original-hash")
    changed = json.loads(subprocess.check_output(args))["sha256"]
    check(changed == sha(dest.read_bytes()) and changed != toc["logical"]["before"]["volume_sha256"], changed)
    return {"refused": sorted(refused), "unaffected": siblings, "changed_data_digest": changed}


def verify(binary, root, native=False):
    layouts = {p.name: verify_layout(binary, root / p.name, p, native) for p in sorted(GOLD.iterdir())
               if (p / "manifest.json").exists()}
    return {"layouts": layouts, **damaged(binary, root)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(tmp), args.native)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
