"""Refuse standalone exports of real UUID-linked VirtualBox VMDK children."""

import argparse
import json
import lzma
from pathlib import Path
import sys
import tempfile

from e2e import check
from e2e_qcow2_backing import checked_import
from e2e_vhd_chain import compare_driver
from e2e_vhdx import digest, native_import, recover

REF = Path(__file__).resolve().parents[1] / "tests/vmdk-vbox-parent"
DRIVER = REF.parent / "vhd-chain/dynamic-flushed"


def verify(binary, root, vbox_img=None):
    manifest = json.loads((REF / "manifest.json").read_text(encoding="utf-8"))
    members = []
    box = None
    if vbox_img:
        sys.path.insert(0, str(REF.parent / "vdi-chain"))
        from capture import VirtualBox, require

        box = VirtualBox(vbox_img, "VMDK")
    try:
        for member in manifest["members"]:
            p = root / member["file"]
            p.write_bytes(lzma.decompress((REF / (p.name + ".xz")).read_bytes()))
            check(digest(p) == member["sha256"], p)
            members.append(p)
            raw = root / "guest.raw"
            reference = DRIVER / (p.stem + ".vhd.guest.raw.xz")
            if len(members) == 1:
                checked_import(
                    binary,
                    [p],
                    raw,
                    ["--raw-sha256", member["guest_sha256"]],
                    kind="vmdk",
                )
                compare_driver(raw, reference)
                raw.unlink()
            else:
                for kind in (None, "vmdk"):
                    native_import(binary, p, raw, kind, expected=4)
            if box:
                with lzma.open(reference) as source, raw.open("xb") as out:
                    measured = box.compare(members, source, out)
                check(digest(raw) == measured == member["guest_sha256"], p)
                if len(members) == len(manifest["members"]):
                    for indexed in (False, True):
                        files = recover(
                            binary,
                            raw,
                            root / ("index" if indexed else "ordinary"),
                            manifest,
                            indexed,
                            folder="deletions",
                        )
                        check(files == manifest["originals"], p)
                raw.unlink()
        # A parent modification UUID alone also precludes standalone conversion.
        base = members[0].read_bytes()
        zero = b"00000000-0000-0000-0000-000000000000"
        for key in (b"ddb.uuid.parent", b"ddb.uuid.parentmodification"):
            at = base.index(key + b'="' + zero + b'"') + len(key) + 2
            changed = root / ("bad-" + key.decode() + ".vmdk")
            changed.write_bytes(base[:at] + b"1" + base[at + 1 :])
            native_import(binary, changed, root / "wrong.raw", "vmdk", expected=4)
        check(
            all(
                digest(p) == m["sha256"]
                for p, m in zip(members, manifest["members"], strict=True)
            ),
            members,
        )
    finally:
        if box:
            require(box.shutdown())
    print(
        "Two real UUID-linked VMDK children refused in automatic and explicit modes; complete standalone base matches Windows",
        flush=True,
    )
    result = {
        "base_sha256": manifest["members"][0]["guest_sha256"],
        "refused": [m["sha256"] for m in manifest["members"][1:]],
    }
    if box:
        result["virtualbox_full_guests"] = [
            m["guest_sha256"] for m in manifest["members"]
        ]
        result["originals"] = files
    return result


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", required=True, type=Path)
    p.add_argument("--vbox-img", type=Path)
    p.add_argument("--output", type=Path)
    a = p.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(a.binary.resolve(), Path(tmp), a.vbox_img)
    if a.output:
        a.output.write_text(
            json.dumps(result, sort_keys=True, indent=2) + "\n", encoding="utf-8"
        )
