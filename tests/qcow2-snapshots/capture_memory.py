"""Save a paused QEMU guest with test-only RAM and an original one-instruction ROM."""
import argparse
import hashlib
import json
import lzma
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
TESTS = HERE.parent


def save_vm(system, disk, work, firmware_directory):
    # Reset vector contains HLT. The test keeps the CPU paused throughout.
    rom = bytearray(65536)
    rom[-16] = 0xf4
    firmware = work / "test.rom"
    firmware.write_bytes(rom)
    cmd = [system, "-accel", "tcg", "-machine", "pc", "-cpu", "qemu64", "-m", "32M",
           "-nodefaults", "-nic", "none", "-display", "none", "-serial", "none", "-S",
           "-bios", firmware, "-drive", f"file={disk},format=qcow2,if=ide", "-qmp", "stdio"]
    if firmware_directory:
        cmd.extend(["-L", firmware_directory])
    with subprocess.Popen(cmd, cwd=work, text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE) as proc:
        def receive():
            line = proc.stdout.readline()
            if not line:
                raise RuntimeError("QEMU exited before returning its QMP result")
            return json.loads(line)

        def request(command, arguments):
            proc.stdin.write(json.dumps({"execute": command, "arguments": arguments}) + "\n")
            proc.stdin.flush()
            while True:
                reply = receive()
                if "return" in reply:
                    return reply["return"]
                if "error" in reply:
                    raise RuntimeError(reply["error"])

        try:
            assert "QMP" in receive()
            request("qmp_capabilities", {})
            assert not request("query-status", {})["running"]
            for operation in ("savevm", "loadvm"):
                result = request("human-monitor-command", {"command-line": operation + " RAM-history"})
                assert result == "", result
            assert not request("query-status", {})["running"]
            request("quit", {})
        finally:
            if proc.poll() is None:
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.terminate()


def capture(base, label, output, qemu, io, system, firmware, temp):
    source = HERE / base
    info = json.loads((source / "manifest.json").read_text(encoding="utf-8"))
    disk = temp / "machine.qcow2"
    with lzma.open(source / "disk.qcow2.xz") as src, disk.open("xb") as dst:
        shutil.copyfileobj(src, dst)
    original = lzma.decompress((TESTS / info["snapshots"][-1]["reference"]).read_bytes())
    expected = original[:-512] + bytes([0x6b]) * 512
    active = original[:-512] + bytes([0xda]) * 512
    offset = len(active) - 512
    subprocess.run([io, "-f", "qcow2", "-c", f"write -P 0x6b {offset} 512", disk], check=True)
    save_vm(system, disk, temp, firmware)
    subprocess.run([io, "-f", "qcow2", "-c", f"write -P 0xda {offset} 512", disk], check=True)
    subprocess.run([qemu, "check", "-f", "qcow2", disk], check=True)
    rows = json.loads(subprocess.check_output([qemu, "info", "--output=json", disk]))["snapshots"]
    vm = next(s for s in rows if s["name"] == "RAM-history")
    assert vm["vm-state-size"] > 0
    output.mkdir(exist_ok=False)
    for flags, oracle in [(["-l", "snapshot.id=" + vm["id"]], expected), ([], active)]:
        raw = temp / "readback.raw"
        subprocess.run([qemu, "convert", *flags, "-O", "raw", disk, raw], check=True)
        assert raw.read_bytes() == oracle
        raw.unlink()
    name = "ram-snapshot-guest.raw.xz"
    (output / name).write_bytes(lzma.compress(expected, preset=3))
    encoded = disk.read_bytes()
    (output / "disk.qcow2.xz").write_bytes(lzma.compress(encoded, preset=3))
    info["snapshots"].append(dict(id=vm["id"], name=vm["name"], guest_bytes=len(expected),
        vm_state_bytes=vm["vm-state-size"], guest_sha256=hashlib.sha256(expected).hexdigest(),
        reference=f"qcow2-snapshots/{label}/{name}"))
    info.update(qemu=subprocess.check_output([qemu, "--version"], text=True).splitlines()[0],
                qemu_system=subprocess.check_output([system, "--version"], text=True).splitlines()[0],
                outer_bytes=len(encoded), image_sha256=hashlib.sha256(encoded).hexdigest(),
                active_sha256=hashlib.sha256(active).hexdigest())
    (output / "manifest.json").write_text(json.dumps(info, indent=2, sort_keys=True, ensure_ascii=False) + "\n",
                                         encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--qemu-img", default="qemu-img")
    parser.add_argument("--qemu-io", default="qemu-io")
    parser.add_argument("--qemu-system", default="qemu-system-x86_64")
    parser.add_argument("--firmware-directory", type=Path)
    args = parser.parse_args()
    for base, label in [("v2-ntfs", "ram-ntfs"), ("extended-ext4", "ram-ext4")]:
        with tempfile.TemporaryDirectory() as tmp:
            capture(base, label, args.output / label, args.qemu_img, args.qemu_io, args.qemu_system,
                    args.firmware_directory, Path(tmp))
