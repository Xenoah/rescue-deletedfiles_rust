"""Detach only a test-created image, with bounded handling of Disk Arbitration.

No device discovery is used to select a target. Both the attachment's exact
whole-disk node and backing image must match before each retry or forced detach.
"""
from pathlib import Path
import plistlib,subprocess,time


def detach_owned_image(image, device, *, sudo=False):
    image=Path(image).resolve(strict=True)
    device=str(device)
    if not device.startswith('/dev/disk') or not device[9:].isdigit():
        raise AssertionError('cleanup requires the exact attached whole-disk node')
    prefix=['sudo'] if sudo else []
    def matches():
        info=subprocess.run([*prefix,'/usr/bin/hdiutil','info','-plist'],capture_output=True,check=True,timeout=15)
        found=[]
        for entry in plistlib.loads(info.stdout).get('images',[]):
            backing=entry.get('image-path')
            nodes={node.get('dev-entry') for node in entry.get('system-entities',[])}
            if device in nodes:
                found.append(backing is not None and Path(backing).resolve()==image)
        if not found:return False
        if found!=[True]:raise AssertionError('fixture cleanup device is attached to a different image')
        return True
    last=''
    for attempt in range(5):
        if not matches():
            # The exact attachment has already disappeared. Never touch a new
            # disk that may later reuse its numeric node.
            return
        args=[*prefix,'/usr/bin/hdiutil','detach',device]
        if attempt==4:args.append('-force')
        result=subprocess.run(args,capture_output=True,text=True,encoding='utf-8',timeout=15)
        if result.returncode==0:
            if matches():raise AssertionError('hdiutil succeeded but the owned attachment remains')
            return
        last=result.stdout+' '+result.stderr
        if 'busy' not in last.lower():raise AssertionError('owned image detach failed: '+last[-2000:])
        if attempt<4:time.sleep(.25*(2**attempt))
    raise AssertionError('owned image remained busy after bounded detach retries: '+last[-2000:])
