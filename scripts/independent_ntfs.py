"""Test ntfs-3g-generated metadata, with an explicitly emulated deletion flag.

Only a fresh regular image inside TemporaryDirectory is formatted/modified.
This is not a Windows deletion test or a real-device test.
"""
import argparse
import hashlib
import json
import re
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path


def command(*args):
    return subprocess.check_output(list(map(str, args)), text=True, encoding="utf-8", stderr=subprocess.STDOUT)


def mark_deleted(image, record_number):
    with image.open("r+b") as f:
        boot = f.read(512)
        sector = struct.unpack_from("<H", boot, 11)[0]
        cluster = sector * boot[13]
        mft = struct.unpack_from("<Q", boot, 48)[0] * cluster
        code = struct.unpack_from("<b", boot, 64)[0]
        record_size = 1 << -code if code < 0 else code * cluster
        f.seek(mft)
        record = bytearray(f.read(record_size))
        usa, count = struct.unpack_from("<HH", record, 4)
        for i in range(1, count):
            record[i*sector-2:i*sector] = record[usa+i*2:usa+i*2+2]
        at = struct.unpack_from("<H", record, 20)[0]
        while at < len(record):
            kind, length = struct.unpack_from("<II", record, at)
            if kind == 0x80 and record[at+9] == 0:
                start = at + struct.unpack_from("<H", record, at+32)[0]
                runs = record[start:at+length]
                break
            assert length > 0 and kind != 0xFFFFFFFF
            at += length
        else:
            raise AssertionError("MFT DATA absent")
        pos, lcn, vcn = 0, 0, 0
        target = record_number * record_size
        physical = None
        while runs[pos]:
            header = runs[pos]
            pos += 1
            width, delta_width = header & 15, header >> 4
            count = int.from_bytes(runs[pos:pos+width], "little")
            pos += width
            assert delta_width > 0
            lcn += int.from_bytes(runs[pos:pos+delta_width], "little", signed=True)
            pos += delta_width
            if vcn*cluster <= target < (vcn+count)*cluster:
                physical = lcn*cluster + target-vcn*cluster
                break
            vcn += count
        assert physical is not None
        f.seek(physical)
        raw = f.read(record_size)
        flags = struct.unpack_from("<H", raw, 22)[0]
        assert raw[:4] == b"FILE" and flags & 1
        sequence = struct.unpack_from("<H", raw, 16)[0]
        f.seek(physical+22)
        f.write(struct.pack("<H", flags & ~1))
        return sequence


def extended_streams(binary, root):
    image=root/'extended.raw'
    with image.open('xb') as f:f.truncate(64*1024*1024)
    command('mkntfs','-F','-Q','-s',512,'-c',4096,image)
    data=root/'extended-data.bin';data.write_bytes(bytes(range(256))*36)
    truth={None:data.read_bytes()};command('ntfscp',image,data,'/attributes.bin')
    for n in range(20):
        name=f'stream-{n:02d}-independently-created';payload=name.encode()+bytes([n])*8192;truth[name]=payload;data.write_bytes(payload)
        command('ntfscp','-N',name,image,data,'/attributes.bin')
    info=command('ntfsinfo','-F','/attributes.bin',image)
    assert '$ATTRIBUTE_LIST (0x20)' in info
    refs=sorted(set(map(int,re.findall(r'from mft record (\d+)',info))))
    assert len(refs)>1
    for ref in refs:mark_deleted(image,ref)
    before=hashlib.sha256(image.read_bytes()).hexdigest()
    summary=json.loads(command(binary,'scan','--source',image,'--work-dir',root/'extended-work','--json'))
    candidates=json.loads(command(binary,'results','--session',summary['session'],'--limit',1000,'--json'))['candidates']
    assert len(candidates)==21 and all(c['supported'] and c['allocation_status']=='conflict' for c in candidates)
    result=json.loads(command(binary,'recover','--session',summary['session'],'--all','--destination',root/'extended-out','--json'))['results']
    names={c['id']:c['stream_name'] for c in candidates}
    assert len(result)==21 and all(Path(r['path']).read_bytes()==truth[names[r['candidate_id']]] for r in result)
    assert hashlib.sha256(image.read_bytes()).hexdigest()==before
    print(json.dumps({'status':'passed','native_attribute_records':len(refs),'byte_exact_streams':21,'scope':'ntfs-3g nonresident attribute list and 20 distinct ADS; explicitly emulated deletion; original allocation bitmap retained and reported as conflict'}))


def main(binary):
    for tool in ("mkntfs", "ntfscp", "ntfsinfo"):
        if shutil.which(tool) is None:
            raise SystemExit(f"Required independent tool missing: {tool}")
    with tempfile.TemporaryDirectory(prefix="rescue-ntfs3g-") as temp:
        root = Path(temp)
        image = root/"volume.raw"
        with image.open("xb") as f:
            f.truncate(64*1024*1024)
        command("mkntfs", "-F", "-Q", "-s", "512", "-c", "4096", image)
        proof = root/"proof.bin"
        proof.write_bytes(bytes(range(256))*100)
        command("ntfscp", image, proof, "/proof.bin")
        ads = root/"ads.bin"
        ads.write_bytes(b"Independent named stream\n" * 100)
        command("ntfscp", "-N", "rescue-proof", image, ads, "/proof.bin")
        info = command("ntfsinfo", "-F", "/proof.bin", image)
        number = int(re.search(r"Dumping Inode (\d+)", info).group(1))
        sequence = mark_deleted(image, number)
        before = hashlib.sha256(image.read_bytes()).hexdigest()
        summary = json.loads(command(binary, "scan", "--source", image, "--work-dir", root/"work", "--json"))
        result = json.loads(command(binary, "recover", "--session", summary["session"],
            "--id", f"mft-{number}-{sequence}", "--destination", root/"out", "--json"))
        recovered = result["results"][0]
        assert recovered["status"] == "completed"
        assert Path(recovered["path"]).read_bytes() == proof.read_bytes()
        candidates = json.loads(command(binary, "results", "--session", summary["session"], "--json"))["candidates"]
        named = next(c for c in candidates if c.get("stream_name") == "rescue-proof")
        result = json.loads(command(binary, "recover", "--session", summary["session"],
            "--id", named["id"], "--destination", root/"out", "--json"))
        assert result["results"][0]["status"] == "completed"
        assert Path(result["results"][0]["path"]).read_bytes() == ads.read_bytes()
        assert hashlib.sha256(image.read_bytes()).hexdigest() == before
        print(json.dumps({"status": "passed", "bytes": recovered["bytes_written"],
                          "ads_bytes": len(ads.read_bytes()),
                          "scope": "ntfs-3g metadata with named DATA; emulated deleted-record flag"}))
        extended_streams(binary,root)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    main(args.binary.resolve())
