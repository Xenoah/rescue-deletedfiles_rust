"""Recover sample-count-delimited FLAC with header/frame CRC and PCM MD5 verification."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
from e2e import check, run

ROOT = Path(__file__).resolve().parents[1]


def data_start(b):
    pos = 4
    while pos + 4 <= len(b):
        last = b[pos] & 128
        pos += 4 + int.from_bytes(b[pos+1:pos+4], 'big')
        if last:
            return pos
    raise ValueError('incomplete FLAC metadata')


def samples():
    manifest = json.loads((ROOT/'tests/flac/manifest.json').read_text())
    files = {name: (ROOT/'tests/flac'/(name+'.flac')).read_bytes() for name in manifest}
    for name, data in files.items():
        check(hashlib.sha256(data).hexdigest() == manifest[name]['sha256'], name)
    files['corpus'] = (ROOT/'tests/corpus/files/sample.flac').read_bytes()
    b = files['mono16']
    files['without-md5'] = b[:26] + bytes(16) + b[42:]
    b = files['stereo24']
    files['streaminfo-only'] = b[:4] + bytes([128]) + b[5:42] + b[data_start(b):]
    return files, manifest


def verify(binary, root, independent=False):
    files, manifest = samples()
    disk = root / 'audio.raw'
    content = bytearray(b'\xb6' * (1024 * 1024 - 1))
    offsets = {}
    for name, b in files.items():
        offsets[len(content)] = name
        content.extend(b)
    disk.write_bytes(content)
    expected = {name: hashlib.sha256(b).hexdigest() for name,b in files.items()}
    for mode, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary,'scan','--source',disk,'--mode','carve','--extension','flac','--work-dir',root/mode,'--json',*flags)
        found = run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(len(found)==len(files) and {c['record'] for c in found}==offsets.keys(),found)
        for c in found:
            name=offsets[c['record']]
            check(c['supported'] and c['size']==len(files[name]) and c['record_sha256']==expected[name],(name,c))
            row,=run(binary,'recover','--session',scan['session'],'--id',c['id'],
                     '--destination',root/(mode+'-out'),'--json')['results']
            output=Path(row['path'])
            check(output.read_bytes()==files[name] and row['sha256']==expected[name],row)
            if independent:
                pcm=subprocess.check_output(['ffmpeg','-nostdin','-loglevel','error','-i',output,
                                             '-f','s32le','-acodec','pcm_s32le','-'])
                original = {'without-md5':'mono16','streaminfo-only':'stereo24'}.get(name,name)
                if original in manifest:
                    args=manifest[original]
                    check(len(pcm)==args['samples']*args['channels']*4 and hashlib.sha256(pcm).hexdigest()==args['pcm_s32_sha256'],name)
                else:
                    check(len(pcm)>0,name)
        print(mode,len(found),'FLAC streams byte-exact',flush=True)
    b=files['stereo16']; start=data_start(b)
    def edit(at, data):
        return b[:at]+data+b[at+len(data):]
    def flip(at):
        return edit(at,bytes([b[at]^1]))
    invalid=[b[:-1],flip(len(b)-2),flip(26),edit(start,b'\0'),flip(start+4),
             edit(4,b'\x01'),edit(5,b'\0\0\x18'),edit(15,b'\0\0\x01'),
             edit(21,bytes([b[21]&240])+bytes(4)),flip(25),edit(start+3,b'\x04')]
    for i, data in enumerate(invalid):
        path=root/f'bad-{i}.flac'
        path.write_bytes(data)
        scan=run(binary,'scan','--source',path,'--mode','carve','--extension','flac','--json',
                 '--work-dir',root/f'bad-{i}',expected=5)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(rows and all(not c['supported'] for c in rows), (i,rows))
    check(disk.read_bytes()==content, 'source changed')
    return expected


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--independent',action='store_true')
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as d:
        verify(args.binary.resolve(),Path(d),args.independent)
