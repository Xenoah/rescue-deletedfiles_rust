"""Validate native installer + portable, then exercise the packaged CLI/MCP."""
import argparse
import hashlib
import json
import os
import plistlib
from installer_versions import versions
from pathlib import Path
import shutil
import struct
import subprocess
import tarfile
import tempfile
import time
import zipfile
import xml.etree.ElementTree as ET
from e2e import check
from e2e_features import main as features
from package import ROOT, VERSION


def architecture(path,platform,arch):
    with path.open('rb') as f:header=f.read(4096)
    if platform=='windows':
        check(header[:2]==b'MZ','not PE');offset=struct.unpack_from('<I',header,60)[0]
        check(header[offset:offset+4]==b'PE\0\0' and struct.unpack_from('<H',header,offset+4)[0]==0x8664,'not AMD64 PE')
        data=path.read_bytes();optional=offset+24
        check(struct.unpack_from('<H',data,optional)[0]==0x20b,'not PE32+')
        sections=struct.unpack_from('<H',data,offset+6)[0];section_at=optional+struct.unpack_from('<H',data,offset+20)[0]
        def physical(rva):
            for i in range(sections):
                at=section_at+i*40;virtual_size,start,raw_size,raw=struct.unpack_from('<IIII',data,at+8)
                if start<=rva<start+max(virtual_size,raw_size):return raw+rva-start
            raise AssertionError('PE RVA outside sections')
        import_rva=struct.unpack_from('<I',data,optional+120)[0];names=[]
        if import_rva:
            at=physical(import_rva)
            while any(data[at:at+20]):
                name=physical(struct.unpack_from('<I',data,at+12)[0]);names.append(data[name:data.index(b'\0',name)].decode('ascii').lower());at+=20
        check(not any(n.startswith(('vcruntime','msvcp')) for n in names),'portable must statically link the VC runtime: '+str(names))
    elif platform=='linux':
        check(header[:6]==b'\x7fELF\x02\x01' and struct.unpack_from('<H',header,18)[0]==62,'not x64 ELF')
    else:
        check(header[:4]==b'\xcf\xfa\xed\xfe' and struct.unpack_from('<I',header,4)[0]==(0x100000c if arch=='arm64' else 0x1000007),'unexpected Mach-O architecture')


def version(binary):
    p=subprocess.run([str(binary),'--version'],capture_output=True,text=True,check=True,timeout=20)
    check(VERSION in p.stdout,p.stdout+p.stderr)


def smoke_gui(gui, log):
    started=time.monotonic()
    with log.open('w') as output:
        process=subprocess.Popen([str(gui)],stdout=output,stderr=output)
        try:
            deadline=started+30
            while True:
                diagnostic=log.read_text(errors='replace')
                check(process.poll() is None,'GUI exited during startup: '+diagnostic)
                renderer=next((line for line in diagnostic.splitlines() if line.startswith('Rescue renderer:')),None)
                if renderer:break
                check(time.monotonic()<deadline,'GUI renderer did not initialize within 30 seconds: '+diagnostic)
                time.sleep(0.1)
            # Measure stability after initialization, including cold virtual hosts.
            time.sleep(3)
            check(process.poll() is None,'GUI exited after renderer initialization: '+log.read_text(errors='replace'))
            return renderer,round(time.monotonic()-started,2)
        finally:
            if process.poll() is None:process.terminate()
            try:process.wait(timeout=10)
            except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)


def main():
    p=argparse.ArgumentParser();p.add_argument('--platform',required=True,choices=['windows','linux','macos']);p.add_argument('--arch',default='x64');p.add_argument('--dist',type=Path,default=ROOT/'dist');p.add_argument('--smoke-gui',action='store_true');a=p.parse_args()
    stem=f'rescue-{VERSION}-{a.platform}-{a.arch}';manifest=json.loads((a.dist/f'package-{a.platform}-{a.arch}.json').read_text())
    for asset in manifest['assets']:
        check(hashlib.sha256((a.dist/asset['file']).read_bytes()).hexdigest()==asset['sha256'],'package checksum mismatch')
    with tempfile.TemporaryDirectory(prefix='rescue-package-') as temp:
        root=Path(temp);portable=root/'portable';portable.mkdir()
        if a.platform=='linux':
            with tarfile.open(a.dist/(stem+'-portable.tar.gz')) as tar:tar.extractall(portable,filter='data')
            binroot=portable/stem
        else:
            with zipfile.ZipFile(a.dist/(stem+'-portable.zip')) as z:
                check(z.testzip() is None,'portable ZIP CRC');z.extractall(portable)
            binroot=portable/'Rescue.app/Contents/MacOS' if a.platform=='macos' else portable
        suffix='.exe' if a.platform=='windows' else ''
        binary=binroot/('rescue'+suffix);gui=binroot/('rescue-gui'+suffix)
        for file in [binary,gui]:
            architecture(file,a.platform,a.arch)
            if a.platform!='windows':file.chmod(file.stat().st_mode|0o755)
        version(binary)
        features(binary)
        gui_status='not_run';renderer_log=None;startup_seconds=None
        if a.smoke_gui:
            renderer_log,startup_seconds=smoke_gui(gui,root/'gui.log')
            if a.platform=='windows':check('Dx12' in renderer_log,'Windows must initialize Direct3D 12: '+renderer_log)
            gui_status='renderer_initialized_then_alive_3_seconds'
        installed=root/'installed'
        if a.platform=='windows':
            setup=a.dist/(stem+'-setup.exe')
            setup_version=subprocess.check_output(['powershell','-NoProfile','-Command',"$version=(Get-Item -LiteralPath $env:RESCUE_SETUP_TEST_PATH).VersionInfo; '{0}.{1}.{2}.{3}' -f $version.FileMajorPart,$version.FileMinorPart,$version.FileBuildPart,$version.FilePrivatePart"],env={**os.environ,'RESCUE_SETUP_TEST_PATH':str(setup.resolve())},text=True).strip()
            check(setup_version==versions(VERSION)['windows'],'Windows installer numeric version mismatch')
            subprocess.run([str(a.dist/(stem+'-setup.exe')),'/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/CURRENTUSER','/DIR='+str(installed)],check=True,timeout=120)
            try:
                version(installed/'rescue.exe')
                for name in ['rescue.exe','rescue-gui.exe','THIRD_PARTY_NOTICES.txt']:
                    check((installed/name).read_bytes()==(binroot/name).read_bytes(),'installer differs from portable: '+name)
            finally:subprocess.run([str(installed/'unins000.exe'),'/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART'],check=True,timeout=120)
            installer_status='installed_version_and_bytes_verified_then_uninstalled'
        elif a.platform=='linux':
            check(subprocess.check_output(['dpkg-deb','--field',str(a.dist/(stem+'.deb')),'Version'],text=True).strip()==versions(VERSION)['linux'],'DEB version mismatch')
            subprocess.run(['dpkg-deb','--extract',str(a.dist/(stem+'.deb')),str(installed)],check=True)
            version(installed/'opt/rescue/rescue')
            for name in ['rescue','rescue-gui','THIRD_PARTY_NOTICES.txt']:check((installed/'opt/rescue'/name).read_bytes()==(binroot/name).read_bytes(),'deb payload differs')
            check(os.readlink(installed/'usr/bin/rescue')=='/opt/rescue/rescue','CLI symlink')
            check((installed/'usr/share/applications/rescue.desktop').is_file(),'desktop entry missing')
            installer_status='deb_extracted_version_payload_and_shortcuts_verified'
        else:
            subprocess.run(['pkgutil','--expand-full',str(a.dist/(stem+'.pkg')),str(installed)],check=True)
            plist=plistlib.loads((portable/'Rescue.app/Contents/Info.plist').read_bytes())
            check(plist['CFBundleVersion']==versions(VERSION)['macos'] and plist['CFBundleShortVersionString']==VERSION.split('-')[0],'macOS numeric/display version mismatch')
            receipts=[ET.parse(p).getroot() for p in installed.rglob('PackageInfo')]
            receipts=[p for p in receipts if p.get('identifier')=='org.xenoah.rescue']
            check(len(receipts)==1 and receipts[0].get('version')==versions(VERSION)['macos'],'PKG receipt version mismatch')
            packaged=list(installed.rglob('Rescue.app/Contents/MacOS/rescue'))
            check(len(packaged)==1,'pkg payload missing or duplicated');version(packaged[0])
            check(packaged[0].read_bytes()==binary.read_bytes(),'pkg and portable CLI differ')
            check((packaged[0].parent/'rescue-gui').read_bytes()==gui.read_bytes(),'pkg and portable GUI differ')
            links=[q for q in installed.rglob('rescue') if q.is_symlink()]
            check(any(os.readlink(q)=='/Applications/Rescue.app/Contents/MacOS/rescue' for q in links),'pkg CLI link missing')
            subprocess.run(['codesign','--verify','--deep','--strict',str(portable/'Rescue.app')],check=True)
            installer_status='pkg_expanded_version_payload_and_symlink_verified_ad_hoc_signature_checked'
        result={'status':'passed','version':VERSION,'platform':a.platform,'arch':a.arch,'portable_cli_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'portable_gui_sha256':hashlib.sha256(gui.read_bytes()).hexdigest(),'installer_validation':installer_status,'packaged_cli_mcp_features':'passed: 35 formats, ZIP bytes/CRC/SHA, date and scope','gui_startup':gui_status,'gui_renderer_log':renderer_log,'gui_startup_seconds':startup_seconds,'manual_gui_test':False}
    (a.dist/f'verification-{a.platform}-{a.arch}.json').write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8');print(json.dumps(result))

if __name__=='__main__':main()
