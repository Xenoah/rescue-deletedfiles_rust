"""Stage only user-installable distributions and their single checksum file.

Native verification records remain in Actions artifacts, never release assets.
"""
import hashlib
from pathlib import Path
import shutil


def package_names(version):
    return [f'rescue-{version}-{platform}-{arch}{suffix}'
            for platform, arch, suffixes in [
                ('windows', 'x64', ('-setup.exe', '-portable.zip')),
                ('linux', 'x64', ('.deb', '-portable.tar.gz')),
                ('macos', 'x64', ('.pkg', '-portable.zip')),
                ('macos', 'arm64', ('.pkg', '-portable.zip'))]
            for suffix in suffixes]


def stage_downloads(source, destination, version):
    source, destination = Path(source), Path(destination)
    names = package_names(version)
    for name in names:
        path = source / name
        if path.is_symlink() or not path.is_file() or not path.stat().st_size:
            raise ValueError('Missing or invalid distribution: ' + name)
    destination.mkdir(parents=True, exist_ok=True)
    if any(destination.iterdir()):
        raise ValueError('Download staging directory must be empty')
    for name in names:
        shutil.copy2(source / name, destination / name)
    (destination / 'SHA256SUMS.txt').write_text(''.join(
        f'{hashlib.sha256((destination / name).read_bytes()).hexdigest()}  {name}\n'
        for name in sorted(names)), encoding='ascii')
    return names
