"""Preserve actual Oracle VirtualBox VMDKs that identify the parent only by its UUID."""

import argparse
import hashlib
import json
import lzma
from pathlib import Path
import struct
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
# This script shares the read-only Oracle disk comparison with the VDI reference suite.
sys.path.insert(0, str(ROOT / "tests/vdi-chain"))
from capture import VirtualBox, require


def capture(box, out):
    ref = ROOT / "tests/vhd-chain/dynamic-flushed"
    original = json.loads((ref / "manifest.json").read_text())
    guests = [
        lzma.decompress((ref / (d["filename"] + ".guest.raw.xz")).read_bytes())
        for d in original["members"]
    ]
    members = []
    out.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory() as temp:
        tmp = Path(temp)
        files = [
            tmp / (Path(d["filename"]).stem + ".vmdk") for d in original["members"]
        ]
        box.capture(files, guests, False, tmp)
        for f, g, spec in zip(files, guests, original["members"], strict=True):
            data = f.read_bytes()
            at, n = struct.unpack_from("<QQ", data, 28)
            descriptor = (
                data[at * 512 : (at + n) * 512].split(b"\0", 1)[0].decode("utf-8")
            )
            fields = dict(
                (k.strip(), v.strip().strip('"'))
                for row in descriptor.splitlines()
                if "=" in row
                for k, v in [row.split("=", 1)]
            )
            assert (
                fields["parentCID"] == "ffffffff" and "parentFileNameHint" not in fields
            )
            assert fields["ddb.uuid.parent"] == (
                members[-1]["uuid"]
                if members
                else "00000000-0000-0000-0000-000000000000"
            )
            assert fields["ddb.uuid.parentmodification"] == (
                members[-1]["modification"]
                if members
                else "00000000-0000-0000-0000-000000000000"
            )
            assert hashlib.sha256(g).hexdigest() == spec["guest_sha256"]
            (out / (f.name + ".xz")).write_bytes(lzma.compress(data, preset=3))
            members.append(
                dict(
                    file=f.name,
                    sha256=hashlib.sha256(data).hexdigest(),
                    uuid=fields["ddb.uuid.image"],
                    modification=fields["ddb.uuid.modification"],
                    guest_sha256=spec["guest_sha256"],
                )
            )
    (out / "manifest.json").write_text(
        json.dumps(
            dict(members=members, originals=original["originals"]),
            sort_keys=True,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vbox-img", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    box = VirtualBox(args.vbox_img, "VMDK")
    try:
        capture(box, args.output)
    finally:
        require(box.shutdown())
