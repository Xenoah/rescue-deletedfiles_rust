"""Generate QEMU hosted-sparse snapshots from independently captured guest generations."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import struct
import subprocess
import tempfile


def sha(data):
    return hashlib.sha256(data).hexdigest()


def capture(case, dest, base_subformat="monolithicSparse"):
    tests = Path(__file__).resolve().parents[1]
    if case == "qemu10-ntfs":
        reference = tests / "vhd-chain/dynamic-flushed"
        info = json.loads((reference / "manifest.json").read_text())
        inputs = [reference / (m["filename"] + ".guest.raw.xz") for m in info["members"]]
    else:
        label, pattern = ("fat32-zero", "fat-*.raw.xz") if case == "qemu6-fat32" else ("ext4-depth8", "ext4-*.raw.xz")
        reference = tests / "vdi-chain" / label
        info = json.loads((reference.parent / "manifest.json").read_text())[label]
        inputs = sorted(reference.glob(pattern))

    dest.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory() as directory:
        work = Path(directory)

        def command(*args):
            return subprocess.check_output(args, cwd=work)

        version = command("qemu-img", "--version").decode().splitlines()[0]
        members = []
        previous = b""
        for i, reference in enumerate(inputs):
            data = lzma.decompress(reference.read_bytes())
            (work / "input.raw").write_bytes(data)
            name = f"child{i}.vmdk" if i else "base.vmdk"
            if i == 0:
                command("qemu-img", "convert", "-f", "raw", "-O", "vmdk", "-o",
                        f"subformat={base_subformat},zeroed_grain=on", "input.raw", name)
            else:
                command("qemu-img", "create", "-f", "vmdk", "-F", "vmdk", "-b", members[-1]["filename"],
                        "-o", "subformat=monolithicSparse,zeroed_grain=on", name)
                writes = []
                for at in range(0, len(data), 65536):
                    span = data[at:at + 65536]
                    if span == previous[at:at + len(span)]:
                        continue
                    if any(span):
                        part = f"sectors-{at}.bin"
                        (work / part).write_bytes(span)
                        writes.extend(["-c", f"write -s {part} {at} {len(span)}"])
                    else:
                        writes.extend(["-c", f"write -z {at} {len(span)}"])
                command("qemu-io", "-f", "vmdk", *writes, name)
            command("qemu-img", "compare", "-f", "raw", "-F", "vmdk", "input.raw", name)
            command("qemu-img", "convert", "-f", "vmdk", "-O", "raw", name, "decoded.raw")
            assert (work / "decoded.raw").read_bytes() == data
            (work / "decoded.raw").unlink()
            chain = json.loads(command("qemu-img", "info", "--backing-chain", "--output=json", name))
            assert len(chain) == i + 1
            image = (work / name).read_bytes()
            start, count = struct.unpack_from("<QQ", image, 28)
            desc = image[start * 512:(start + count) * 512].split(b"\0", 1)[0].decode()
            fields = dict((k.strip(), v.strip().strip('"')) for line in desc.splitlines()
                          if "=" in line for k, v in [line.split("=", 1)])
            if i:
                assert int(fields["parentCID"], 16) == int(members[-1]["cid"], 16)
                assert fields["parentFileNameHint"] == members[-1]["filename"]
            members.append(dict(filename=name, cid=fields["CID"], parent_cid=fields["parentCID"],
                                guest_sha256=sha(data), guest_bytes=len(data), sha256=sha(image),
                                parent_name=fields.get("parentFileNameHint"), reference=str(reference.relative_to(tests))))
            (dest / (name + ".xz")).write_bytes(lzma.compress(image, preset=3))
            previous = data
    (dest / "manifest.json").write_text(json.dumps(dict(qemu=version, members=members, originals=info["originals"]),
                                                  sort_keys=True, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("case", choices=["qemu10-ntfs", "qemu6-fat32", "qemu10-ext4"])
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    capture(args.case, args.destination)
