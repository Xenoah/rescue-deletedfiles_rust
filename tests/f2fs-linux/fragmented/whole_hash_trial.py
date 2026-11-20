"""Test bounded regular fragmentation using only a header, file length and original SHA-256."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import subprocess
import tempfile

SIGNATURES = {"bmp": b"BM", "jpg": b"\xff\xd8\xff", "png": b"\x89PNG\r\n\x1a\n", "sqlite": b"SQLite format 3\0",
              "wav": b"RIFF", "zip": b"PK\x03\x04"}

def verify(binary, base, root):
    disk = root / "disk.raw"
    data = lzma.decompress((base / "disk.raw.xz").read_bytes())
    disk.write_bytes(data)
    result = {}
    for ext, magic in SIGNATURES.items():
        original = (base / "files" / ("original." + ext)).read_bytes()
        sha = hashlib.sha256(original).hexdigest()
        candidates = [pos for pos in range(0, len(data), 4096) if data.startswith(magic, pos)
                      and (ext != "wav" or data[pos + 8:pos + 12] == b"WAVE")]
        row = dict(header_offsets=candidates, exact=False, original_sha256=sha, outcomes=[])
        for start in candidates:
            target = root / ("recovered." + ext)
            cmd = [binary, "recover-by-hash", "--source", disk, "--output", target, "--sha256", sha, "--file-bytes", str(len(original)),
                   "--block-size", "4096", "--first-offset", str(start), "--max-stride-blocks", "8", "--run-blocks", "1", "--json"]
            proc = subprocess.run(cmd, capture_output=True, timeout=120)
            record = json.loads(proc.stdout or proc.stderr)
            if proc.returncode == 0:
                assert target.read_bytes() == original and hashlib.sha256(target.read_bytes()).hexdigest() == sha
                row.update(exact=True, selected_header=start, detected_stride=record["matched_stride"]["stride_blocks"])
                row["outcomes"].append("matched")
                target.unlink()
                break
            assert not target.exists()
            row["outcomes"].append(record["error"]["code"])
        result[ext] = row
    assert hashlib.sha256(disk.read_bytes()).hexdigest() == hashlib.sha256(data).hexdigest()
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp:
        result = verify(args.binary.resolve(), Path(__file__).resolve().parent, Path(tmp))
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print({key:val["exact"] for key,val in result.items()})
