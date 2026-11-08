"""Numeric installer builds must increase from previews to the stable release.

Display versions remain SemVer. macOS gets three numeric components within
Apple's documented 4/2/2 digit limits; Windows gets a four-component resource.
"""
import re


def versions(version):
    match=re.fullmatch(r'(\d+)\.(\d+)\.(\d+)(?:-alpha\.(\d+))?',version)
    if not match:raise ValueError('unsupported installer version: '+version)
    major,minor,patch=map(int,match.group(1,2,3));preview=match.group(4)
    if any(part>99 for part in [major,minor,patch]) or major*100+minor==0:
        raise ValueError('installer version components exceed the supported numeric range')
    stage=int(preview) if preview is not None else 99
    if preview is not None and not 1<=stage<99:raise ValueError('alpha installer ordinal must be 1..98')
    return {'windows':f'{major}.{minor}.{patch}.{stage}',
            'macos':f'{major*100+minor}.{patch}.{stage}',
            'linux':version.replace('-alpha.','~alpha.')}
