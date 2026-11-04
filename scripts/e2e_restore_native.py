"""Actual deletion and same-volume restoration on newly created virtual media.

Formatting/attaching is restricted to a unique image created by this process.
No existing host disk is selected or formatted. Linux/macOS use exFAT; Windows
uses NTFS. Native source handles in Rescue remain read-only.
"""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import shlex
import subprocess
import tempfile
import time
import uuid
from e2e import Client, check, run

ROOT = Path(__file__).resolve().parent.parent


def call(*args, env=None):
    p = subprocess.run(list(map(str, args)), env=env, capture_output=True,
                       text=True, encoding='utf-8', timeout=180)
    check(p.returncode == 0, f'{args[0]} failed: {p.stdout[-2000:]} {p.stderr[-2000:]}')
    return p.stdout.strip()


@contextmanager
def mounted_fixture(root):
    system = platform.system()
    if system == 'Linux':
        image = root/'owned-volume.raw'
        with image.open('xb') as f:
            f.truncate(64*1024*1024)
        call('mkfs.exfat', '-c', '4096', image)
        device = call('sudo', 'losetup', '--find', '--show', image)
        check(device.startswith('/dev/loop') and device[9:].isdigit(), 'unexpected fixture node')
        mount = root/'owned-mount'
        mount.mkdir()
        attached = False
        try:
            call('sudo', 'mount.exfat-fuse', '-o', f'uid={os.getuid()},gid={os.getgid()}', device, mount)
            attached = True
            yield Path(device), mount, 'exfat'
        finally:
            if attached:
                call('sudo', 'umount', mount)
            call('sudo', 'losetup', '--detach', device)
    elif system == 'Darwin':
        image = root/'owned-volume.dmg'
        label = 'RESCUE_'+uuid.uuid4().hex[:8]
        call('sudo', '/usr/bin/hdiutil', 'create', '-size', '64m', '-fs', 'ExFAT', '-volname', label,
             '-layout', 'MBRSPUD', image)
        p = subprocess.run(['sudo', '/usr/bin/hdiutil', 'attach', '-nobrowse', '-plist', str(image)],
                           capture_output=True, check=True, timeout=120)
        nodes = plistlib.loads(p.stdout)['system-entities']
        mounted = next(n for n in nodes if n.get('mount-point'))
        whole = next(n['dev-entry'] for n in nodes if n.get('dev-entry', '').startswith('/dev/disk') and n['dev-entry'][9:].isdigit())
        try:
            call('sudo','chmod','a+rwx',mounted['mount-point'])
            yield Path('/dev/r'+mounted['dev-entry'][5:]), Path(mounted['mount-point']), 'exfat'
        finally:
            call('sudo','/usr/bin/hdiutil', 'detach', whole)
    elif system == 'Windows':
        image = root/'owned-volume.vhd'
        check(not image.exists() and image.is_absolute(), 'fixture must be new and absolute')
        script = root/'owned-diskpart.txt'
        script.write_text(f'create vdisk file="{image}" maximum=96 type=fixed\n'
                          f'select vdisk file="{image}"\nattach vdisk\nconvert mbr\n'
                          'create partition primary\nformat fs=ntfs quick label=RESCUE_OWNED\nassign\n', encoding='utf-8')
        env = os.environ.copy()
        env['RESCUE_OWNED_IMAGE'] = str(image)
        def ps(code):
            return call('powershell.exe', '-NoLogo', '-NoProfile', '-NonInteractive', '-Command',
                        "$ErrorActionPreference='Stop';"+code, env=env)
        call('diskpart.exe', '/s', script)
        try:
            value = json.loads(ps('$i=Get-DiskImage -ImagePath $env:RESCUE_OWNED_IMAGE; '
                                 'if(-not $i.Attached){throw "owned fixture not attached"}; '
                                 '$d=$i|Get-Disk; $p=Get-Partition -DiskNumber $d.Number | '
                                 'Where-Object DriveLetter | Select-Object -First 1; '
                                 'if(-not $p){throw "owned fixture has no mount"}; '
                                 '@{source=("\\\\.\\"+$p.DriveLetter+":");root=($p.DriveLetter+":\\")} | ConvertTo-Json -Compress'))
            yield Path(value['source']), Path(value['root']), 'ntfs'
        finally:
            ps('Dismount-DiskImage -ImagePath $env:RESCUE_OWNED_IMAGE | Out-Null')
    else:
        raise RuntimeError('unsupported platform')


def sync_volume(mount):
    if platform.system() == 'Linux':
        call('sync', '-f', mount)
    elif platform.system() == 'Darwin':
        call('/bin/sync')
    # Windows CloseHandle after fsync flushes each test file; directory deletion
    # is visible through shared native volume handles without dismounting.


def deleted_file(mount, name, data):
    path = mount/name
    with path.open('xb') as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    path.unlink()
    sync_volume(mount)


def main(binary, output):
    with tempfile.TemporaryDirectory(prefix='rescue-owned-restore-') as temp:
        root = Path(temp).resolve()
        data = (ROOT/'tests/corpus/files/sample.pdf').read_bytes()
        executable = binary
        if platform.system() != 'Windows':
            executable = root/'rescue-native'
            executable.write_text('#!/bin/sh\nexec sudo '+shlex.quote(str(binary))+' "$@"\n', encoding='utf-8')
            executable.chmod(0o755)
        with mounted_fixture(root) as (source, mount, filesystem):
            deleted_file(mount, 'native-report.pdf', data)
            restored = run(executable, 'restore', '--source', source, '--name', 'native-report.pdf', '--original', '--json')
            file = restored['results'][0]
            check(file['status'] == 'completed' and Path(file['path']).samefile(mount/'native-report.pdf'), restored)
            check((mount/'native-report.pdf').read_bytes() == data, 'native original PDF bytes differ')
            # Second deletion through MCP. The nonexistent work directory is on
            # the source volume and must stay nonexistent throughout scanning.
            deleted_file(mount, 'native-mcp.pdf', data)
            work = mount/'must-not-create-work'
            client = Client(executable, source, work, mount)
            try:
                client.rpc('initialize', {'protocolVersion': '2025-11-25'})
                scanned = client.wait(client.tool('start_scan', {'source': str(source), 'request_id': 'native-scan'})['job_id'])
                check(scanned['state'] == 'completed' and scanned['result']['session_file'] is None and not work.exists(), scanned)
                sid = scanned['result']['session_id']
                candidates = client.tool('list_candidates', {'session_id': sid})['candidates']
                candidate = next(c for c in candidates if c['name'] == 'native-mcp.pdf')
                recovered = client.wait(client.tool('recover_files', {'session_id': sid, 'ids': [candidate['id']],
                                            'output_format': 'original', 'request_id': 'native-original'})['job_id'])
                check(recovered['state'] == 'completed' and (mount/'native-mcp.pdf').read_bytes() == data and not work.exists(), recovered)
            finally:
                client.close()
            result = {'schema_version': 1, 'status': 'passed', 'version': call(executable, '--version'),
                      'os': platform.system(), 'architecture': platform.machine(), 'filesystem': filesystem,
                      'native_same_volume': True, 'actual_deletion': True, 'original_root_auto_detected': True,
                      'cli_pdf_byte_exact': True, 'mcp_pdf_byte_exact': True, 'session_work_never_created': True,
                      'pdf_sha256': hashlib.sha256(data).hexdigest(), 'pdf_bytes': len(data),
                      'scope': 'newly created writable virtual volume only; actual deletion; no host disk formatted'}
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
        print(json.dumps(result))


def preflight(output):
    with tempfile.TemporaryDirectory(prefix='rescue-owned-preflight-') as temporary:
        with mounted_fixture(Path(temporary).resolve()) as (source,mount,filesystem):
            deleted_file(mount,'fixture-preflight.pdf',b'%PDF-1.4\nfixture mount check\n%%EOF\n')
            check(not (mount/'fixture-preflight.pdf').exists(),'fixture deletion failed')
            proof={'status':'passed','scope':'virtual fixture setup only; not recovery evidence','os':platform.system(),'filesystem':filesystem,'source':str(source),'mount':str(mount)}
    output.parent.mkdir(parents=True,exist_ok=True)
    output.write_text(json.dumps(proof,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(proof))


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path)
    p.add_argument('--fixture-only',action='store_true')
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    if a.fixture_only:preflight(a.output.resolve())
    elif a.binary:main(a.binary.resolve(), a.output.resolve())
    else:p.error('--binary is required for recovery verification')
