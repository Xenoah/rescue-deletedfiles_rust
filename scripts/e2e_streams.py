"""NTFS LZNT1 and ADS integration, including native Windows compression as an oracle.

Synthetic metadata only. All mutations are limited to fresh test images.
"""
import argparse
import ctypes
import hashlib
import json
import struct
import sys
import tempfile
from pathlib import Path
from e2e import Client, check, run
from make_fixture import build, record, filename, resident, nonresident, MFT_LCN, SECTOR, RECORD


def ads_id(number, name):
    codes = struct.unpack("<" + "H" * (len(name.encode("utf-16le")) // 2), name.encode("utf-16le"))
    return f"mft-{number}-7-ads-" + "".join(f"{code:04x}" for code in codes)


def repeat_chunk(byte):
    return bytes([3, 0xB0, 2, byte, 0xFC, 0x0F])


def windows_compress(data):
    ntdll = ctypes.WinDLL("ntdll")
    ulong = ctypes.c_uint32
    ntdll.RtlGetCompressionWorkSpaceSize.argtypes = [ctypes.c_ushort, ctypes.POINTER(ulong), ctypes.POINTER(ulong)]
    ntdll.RtlGetCompressionWorkSpaceSize.restype = ctypes.c_int32
    ntdll.RtlCompressBuffer.argtypes = [ctypes.c_ushort, ctypes.c_void_p, ulong,
        ctypes.c_void_p, ulong, ulong, ctypes.POINTER(ulong), ctypes.c_void_p]
    ntdll.RtlCompressBuffer.restype = ctypes.c_int32
    work_size, fragment_size, written = ulong(), ulong(), ulong()
    check(ntdll.RtlGetCompressionWorkSpaceSize(2, ctypes.byref(work_size), ctypes.byref(fragment_size)) == 0, "native workspace")
    workspace = ctypes.create_string_buffer(work_size.value)
    source = ctypes.create_string_buffer(data)
    output = ctypes.create_string_buffer(len(data)*2+4096)
    status = ntdll.RtlCompressBuffer(2, source, len(data), output, len(output), 4096,
                                  ctypes.byref(written), workspace)
    check(status == 0, f"RtlCompressBuffer: {status:#x}")
    return output.raw[:written.value]


def extension_fixture(path):
    truth = build(path)
    volume = bytearray(path.read_bytes())

    def put_record(index, attributes):
        start = MFT_LCN*SECTOR + index*RECORD
        volume[start:start+RECORD] = record(index, attributes)

    def payload(lcn, data):
        volume[lcn*SECTOR:lcn*SECTOR+len(data)] = data

    def expect(index, name, data, stream=None):
        key = f"mft-{index}-7" if stream is None else ads_id(index, stream)
        truth[key] = {"name": name, "hex": data.hex(), "size": len(data),
                      "sha256": hashlib.sha256(data).hexdigest()}

    plain = bytes((i*71 + i//17) % 256 for i in range(8192))
    expected = b"A"*4096 + b"B"*4096 + plain + b"\0"*8192 + b"Z"*500 + b"\0"*500
    put_record(30, [filename("compressed.bin"), nonresident(
        [(1, 400), (15, None), (16, 420), (16, None), (1, 450), (15, None)],
        len(expected), initialized=len(expected)-500, flags=1, compression=4)])
    payload(400, repeat_chunk(65)+repeat_chunk(66))
    payload(420, plain)
    payload(450, repeat_chunk(90)*2)
    expect(30, "compressed.bin", expected)

    japanese = "補足データ"
    traversal = "../../escape:$DATA"
    ads_bytes = b"ADS-first"*64 + b"tail"
    attributes = [filename("streams.txt"), resident(0x80, b"main content"),
        resident(0x80, b"[ZoneTransfer]\r\nZoneId=3\r\n", name="Zone.Identifier"),
        nonresident([(1, 460), (1, 458)], len(ads_bytes), name=japanese),
        resident(0x80, b"keep this a flat filename", name=traversal),
        nonresident([(1, 465), (15, None)], 4096, flags=1, compression=4, name="compressed")]
    payload(460, ads_bytes[:512])
    payload(458, ads_bytes[512:])
    payload(465, repeat_chunk(67))
    put_record(31, attributes)
    expect(31, "streams.txt", b"main content")
    for name, data in [("Zone.Identifier", b"[ZoneTransfer]\r\nZoneId=3\r\n"),
                       (japanese, ads_bytes), (traversal, b"keep this a flat filename"),
                       ("compressed", b"C"*4096)]:
        expect(31, f"streams.txt:{name}", data, name)

    # A compressed unit crossing a discontinuity in its physical allocation.
    # 520 literal bytes plus a match gives >512 encoded bytes, requiring both runs.
    head = bytes((i*13) % 256 for i in range(520))
    body = bytearray()
    for at in range(0, len(head), 8):
        body.extend(b"\0" + head[at:at+8])
    # At output position 520: 10 distance bits, 6 length bits.
    body.extend(b"\x01" + struct.pack("<H", (1 << 6) | 61))  # distance=2, length=64
    encoded = struct.pack("<H", 0xB000 | (len(body)-1)) + body
    fragmented_truth = head + head[-2:]*32
    put_record(33, [filename("fragmented-compression.bin"), nonresident(
        [(1, 470), (1, 468), (14, None)], len(fragmented_truth), flags=1, compression=4)])
    payload(470, encoded[:512])
    payload(468, encoded[512:])
    expect(33, "fragmented-compression.bin", fragmented_truth)

    native = 0
    if sys.platform == "win32":
        # Native encoder exercises changing displacement/length widths, chunk resets,
        # overlapping copies and nontrivial literal sequences independently of our parser.
        for index, size in enumerate((64, 4096, 8192), start=34):
            sample = (bytes(range(251))*3 + b"ABCD"*2000)[:size]
            encoded = windows_compress(sample)
            clusters = (len(encoded)+511)//512
            check(0 < clusters < 16, "oracle fixture must actually be compressed")
            lcn = 500 + (index-34)*16
            put_record(index, [filename(f"windows-lznt1-{size}.bin"), nonresident(
                [(clusters, lcn), (16-clusters, None)], len(sample), flags=1, compression=4)])
            payload(lcn, encoded)
            expect(index, f"windows-lznt1-{size}.bin", sample)
            native += 1

    path.write_bytes(volume)
    return truth, native


def main(binary):
    with tempfile.TemporaryDirectory(prefix="rescue-streams-") as temporary:
        root = Path(temporary)
        source = root/"source.raw"
        truth, native = extension_fixture(source)
        before = hashlib.sha256(source.read_bytes()).hexdigest()
        scan = run(binary, "scan", "--source", source, "--work-dir", root/"work", "--json")
        session = Path(scan["session"])
        candidates = run(binary, "results", "--session", session, "--json")["candidates"]
        check({c["id"] for c in candidates} == set(truth), "ADS enumeration or live file leak")
        check(all(c["supported"] for c in candidates), "supported compression rejected")
        check(sum(c["stream_name"] is not None for c in candidates) == 4, "stream names missing")
        result = run(binary, "recover", "--session", session, "--all", "--destination", root/"out", "--json")
        for item in result["results"]:
            recovered = Path(item["path"])
            check(recovered.parent.samefile(root/"out"), "ADS escaped destination")
            check(":" not in recovered.name, "ADS became an actual output stream")
            check(recovered.read_bytes().hex() == truth[item["candidate_id"]]["hex"], item)
            check(item["sha256"] == truth[item["candidate_id"]]["sha256"], item)
        check(hashlib.sha256(source.read_bytes()).hexdigest() == before, "source changed")

        legacy = json.loads(session.read_text(encoding="utf-8"))
        legacy["schema_version"] = 1
        legacy["candidates"] = [c for c in legacy["candidates"] if c["stream_name"] is None]
        for candidate in legacy["candidates"]:
            candidate.pop("stream_name")
        legacy_path = root/"legacy.json"
        legacy_path.write_text(json.dumps(legacy), encoding="utf-8")
        result = run(binary, "recover", "--session", legacy_path, "--id", "mft-20-7", "--destination", root/"legacy-out", "--json")
        check(Path(result["results"][0]["path"]).read_bytes().hex() == truth["mft-20-7"]["hex"], "v1 session regression")

        # Stream identity is checked against the record and candidate ID on restore.
        edited = json.loads(session.read_text(encoding="utf-8"))
        ads = next(c for c in edited["candidates"] if c["stream_name"] == "Zone.Identifier")
        ads["stream_name"] = "compressed"
        tampered = root/"tampered.json"
        tampered.write_text(json.dumps(edited), encoding="utf-8")
        result = run(binary, "recover", "--session", tampered, "--id", ads["id"], "--destination", root/"tampered-out", "--json", expected=5)
        check(result["results"][0]["path"] is None, "tampered stream was restored")

        client = Client(binary, source, root/"mcp-work", root/"mcp-out")
        try:
            client.rpc("initialize", {"protocolVersion": "2025-11-25"})
            job = client.wait(client.tool("start_scan", {"source": str(source), "request_id": "scan-streams"})["job_id"])
            sid = job["result"]["session_id"]
            ids = [ads_id(31, "compressed"), "mft-30-7"]
            job = client.wait(client.tool("recover_files", {"session_id": sid, "ids": ids,
                "destination": str(root/"mcp-out"), "request_id": "recover-streams"})["job_id"])
            check(job["state"] == "completed", job)
            for item in job["result"]["results"]:
                check(Path(item["path"]).read_bytes().hex() == truth[item["candidate_id"]]["hex"], "MCP stream mismatch")
        finally:
            client.close()

        # Corrupt compressed payload: no successful/zero-filled result may be published.
        with source.open("r+b") as f:
            f.seek(400*SECTOR)
            f.write(bytes([2, 0xB0, 1, 0, 0]))
            f.seek(MFT_LCN*SECTOR + 32*RECORD)
            f.write(record(32, [filename("mixed-support.txt"), resident(0x80, b"clear"),
                nonresident([(1, 480)], 512, flags=0x4000, name="encrypted")]))
        scan = run(binary, "scan", "--source", source, "--work-dir", root/"bad-work", "--json")
        mixed = run(binary, "results", "--session", scan["session"], "--name", "mixed-support", "--json")["candidates"]
        check(len(mixed) == 2 and sum(c["supported"] for c in mixed) == 1, "unsupported ADS blocked the main stream")
        result = run(binary, "recover", "--session", scan["session"], "--id", "mft-30-7", "--destination", root/"bad-out", "--json", expected=5)
        item = result["results"][0]
        check(item["status"] == "incomplete" and item["sha256"] is None, item)
        check(Path(item["path"]).suffix == ".partial", "corrupt stream published as complete")
        print(json.dumps({"status": "passed", "byte_exact_streams": len(truth), "native_windows_vectors": native,
                          "scope": "LZNT1 mixed/sparse/fragmented units, ADS, MCP, tamper and corrupt-data rejection"}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    main(parser.parse_args().binary.resolve())
