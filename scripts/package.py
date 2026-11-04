"""Package native, tested executables into installer and portable distributions.

Run on the matching OS. Packaging does not generate recovery test evidence.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tarfile
import tomllib
import zipfile

ROOT=Path(__file__).resolve().parent.parent
VERSION=tomllib.loads((ROOT/'Cargo.toml').read_text())['workspace']['package']['version']
TARGETS={('windows','x64'):'x86_64-pc-windows-msvc',('linux','x64'):'x86_64-unknown-linux-gnu',('macos','x64'):'x86_64-apple-darwin',('macos','arm64'):'aarch64-apple-darwin'}


def command(*args):
    subprocess.run(list(map(str,args)),check=True)


def copy_docs(target,notices):
    target.mkdir(parents=True,exist_ok=True)
    for name in ['README.md','LICENSE']:
        shutil.copy2(ROOT/name,target/name)
    shutil.copy2(notices,target/'THIRD_PARTY_NOTICES.txt')
    shutil.copytree(ROOT/'docs',target/'docs')


def permissions(root,executables):
    for path in [root,*root.rglob('*')]:
        if path.is_symlink():continue
        path.chmod(0o755 if path.is_dir() or path in executables else 0o644)


def zip_tree(source,output):
    with zipfile.ZipFile(output,'x',zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(source.rglob('*')):
            if path.is_file():archive.write(path,path.relative_to(source).as_posix())


def main():
    p=argparse.ArgumentParser();p.add_argument('--validate-tag');p.add_argument('--platform',choices=['windows','linux','macos']);p.add_argument('--arch',choices=['x64','arm64'],default='x64');p.add_argument('--output',type=Path,default=ROOT/'dist');p.add_argument('--binaries',type=Path,default=ROOT/'target/release');p.add_argument('--notices',type=Path,default=ROOT/'dist/THIRD_PARTY_NOTICES.txt');a=p.parse_args()
    if a.validate_tag:
        if a.validate_tag!='v'+VERSION:raise SystemExit(f'Tag {a.validate_tag} differs from workspace {VERSION}')
        return
    if (a.platform,a.arch) not in TARGETS:raise SystemExit('Choose a supported native platform and architecture')
    dist=a.output.resolve();dist.mkdir(parents=True,exist_ok=True);stem=f'rescue-{VERSION}-{a.platform}-{a.arch}'
    stage=dist/'stage'/stem
    if stage.exists():raise SystemExit('Package staging already exists: '+str(stage))
    stage.mkdir(parents=True)
    suffix='.exe' if a.platform=='windows' else ''
    for name in ['rescue','rescue-gui']:
        if not (a.binaries/(name+suffix)).is_file():raise SystemExit('Missing native executable '+name)
    os.umask(0o022)
    assets=[]
    if a.platform in ['windows','linux']:
        portable=stage/stem;copy_docs(portable,a.notices)
        for name in ['rescue','rescue-gui']:shutil.copy2(a.binaries/(name+suffix),portable/(name+suffix))
        if a.platform=='windows':
            archive=dist/(stem+'-portable.zip');zip_tree(portable,archive);assets.append(archive)
            installer=dist/(stem+'-setup.exe')
            script=stage/'setup.iss'
            script.write_text(f'''[Setup]
AppId={{{{CF1492CA-68D8-40E9-994B-916F3344769E}}
AppName=Rescue
AppVersion={VERSION}
VersionInfoVersion={VERSION.replace("-alpha.", ".")}
AppPublisher=Xenoah
AppPublisherURL=https://github.com/Xenoah/rescue-deletedfiles_rust
DefaultDirName={{localappdata}}\\Programs\\Rescue
DefaultGroupName=Rescue
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={{app}}\\rescue-gui.exe
OutputDir={dist}
OutputBaseFilename={stem}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile={ROOT/'LICENSE'}
[Files]
Source: "{portable}\\*"; DestDir: "{{app}}"; Flags: ignoreversion recursesubdirs createallsubdirs
[Icons]
Name: "{{group}}\\Rescue"; Filename: "{{app}}\\rescue-gui.exe"
Name: "{{group}}\\Uninstall Rescue"; Filename: "{{uninstallexe}}"
[Run]
Filename: "{{app}}\\rescue-gui.exe"; Description: "Launch Rescue"; Flags: nowait postinstall skipifsilent
''',encoding='utf-8-sig')
            iscc=shutil.which('ISCC') or str(Path(os.environ.get('ProgramFiles(x86)',r'C:\Program Files (x86)'))/'Inno Setup 6'/'ISCC.exe')
            command(iscc,'/Qp',script);assets.append(installer)
        else:
            permissions(portable,{portable/'rescue',portable/'rescue-gui'})
            archive=dist/(stem+'-portable.tar.gz')
            with tarfile.open(archive,'x:gz') as tar:tar.add(portable,arcname=stem)
            assets.append(archive)
            debroot=stage/'deb';(debroot/'opt').mkdir(parents=True);shutil.copytree(portable,debroot/'opt/rescue')
            control=debroot/'DEBIAN';control.mkdir()
            control.joinpath('control').write_text(f'''Package: rescue-deletedfiles
Version: {VERSION.replace('-alpha.','~alpha.')}
Section: utils
Priority: optional
Architecture: amd64
Maintainer: Xenoah <noreply@users.noreply.github.com>
Homepage: https://github.com/Xenoah/rescue-deletedfiles_rust
Depends: libc6 (>= 2.35), libgcc-s1, libstdc++6, libx11-6, libxcursor1, libxrandr2, libxi6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libgl1, libegl1
Recommends: xdg-desktop-portal, xdg-desktop-portal-gtk, fonts-noto-cjk
Description: Read-only disk recovery GUI, CLI and MCP server
 NTFS, FAT32, exFAT metadata recovery and contiguous PNG carving.
''',encoding='utf-8')
            (debroot/'usr/bin').mkdir(parents=True)
            for name in ['rescue','rescue-gui']:(debroot/'usr/bin'/name).symlink_to('/opt/rescue/'+name)
            desktop=debroot/'usr/share/applications';desktop.mkdir(parents=True)
            (desktop/'rescue.desktop').write_text('[Desktop Entry]\nType=Application\nName=Rescue\nComment=Recover deleted files from disks\nExec=/opt/rescue/rescue-gui\nIcon=rescue\nTerminal=false\nCategories=Utility;System;\n',encoding='utf-8')
            icons=debroot/'usr/share/icons/hicolor/scalable/apps';icons.mkdir(parents=True)
            (icons/'rescue.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128"><rect width="128" height="128" rx="24" fill="#191919"/><path d="M35 92V32h32q28 0 28 21 0 15-16 20l20 19H78L58 73h-6v19zm17-35h15q10 0 10-6t-10-6H52z" fill="#70c7ff"/></svg>')
            permissions(debroot,{debroot/'opt/rescue/rescue',debroot/'opt/rescue/rescue-gui'})
            installer=dist/(stem+'.deb');command('dpkg-deb','--root-owner-group','--build',debroot,installer);assets.append(installer)
    else:
        app=stage/'Rescue.app';macos=app/'Contents/MacOS';macos.mkdir(parents=True)
        resources=app/'Contents/Resources';copy_docs(resources,a.notices)
        for name in ['rescue','rescue-gui']:shutil.copy2(a.binaries/name,macos/name)
        build_version='.'.join(VERSION.split('-')[0].split('.')[:2]+[VERSION.rsplit('.',1)[1]])
        plist={'CFBundleName':'Rescue','CFBundleDisplayName':'Rescue','CFBundleIdentifier':'org.xenoah.rescue','CFBundleExecutable':'rescue-gui','CFBundlePackageType':'APPL','CFBundleShortVersionString':VERSION.split('-')[0],'CFBundleVersion':build_version,'LSMinimumSystemVersion':'13.0','NSHighResolutionCapable':True,'NSHumanReadableCopyright':'MIT License · Xenoah'}
        with (app/'Contents/Info.plist').open('wb') as f:plistlib.dump(plist,f)
        permissions(app,{macos/'rescue',macos/'rescue-gui'})
        for name in ['rescue','rescue-gui']:command('codesign','--force','--sign','-',macos/name)
        command('codesign','--force','--sign','-',app)
        command('codesign','--verify','--deep','--strict',app)
        archive=dist/(stem+'-portable.zip');command('ditto','-c','-k','--sequesterRsrc','--keepParent',app,archive);assets.append(archive)
        pkgroot=stage/'pkg';(pkgroot/'Applications').mkdir(parents=True);shutil.copytree(app,pkgroot/'Applications/Rescue.app',symlinks=True)
        (pkgroot/'usr/local/bin').mkdir(parents=True);(pkgroot/'usr/local/bin/rescue').symlink_to('/Applications/Rescue.app/Contents/MacOS/rescue')
        installer=dist/(stem+'.pkg');command('pkgbuild','--root',pkgroot,'--identifier','org.xenoah.rescue','--version',build_version,'--install-location','/','--ownership','recommended',installer);assets.append(installer)
    manifest=dist/f'package-{a.platform}-{a.arch}.json'
    manifest.write_text(json.dumps({'version':VERSION,'platform':a.platform,'arch':a.arch,'target':TARGETS[a.platform,a.arch],'publisher_signed':False,'macos_ad_hoc_signed':a.platform=='macos','macos_notarized':False,'assets':[{'file':f.name,'sha256':hashlib.sha256(f.read_bytes()).hexdigest(),'bytes':f.stat().st_size} for f in assets]},indent=2)+'\n',encoding='utf-8');assets.append(manifest)
    (dist/f'SHA256SUMS-{a.platform}-{a.arch}.txt').write_text(''.join(f'{hashlib.sha256(f.read_bytes()).hexdigest()}  {f.name}\n' for f in assets),encoding='ascii')
    print(json.dumps({'status':'packaged','assets':[f.name for f in assets]}))

if __name__=='__main__':main()
