"""Carve encoded MPEG Layer III files and verify Xing/Info frame counts and byte extents."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
from e2e import check, run

ROOT = Path(__file__).resolve().parents[1]


def samples():
    manifest = json.loads((ROOT/'tests/mp3/manifest.json').read_text())
    files = {name: (ROOT/'tests/mp3'/(name+'.mp3')).read_bytes() for name in manifest}
    for name, b in files.items():
        check(hashlib.sha256(b).hexdigest() == manifest[name]['sha256'], name)
    files['corpus'] = (ROOT/'tests/corpus/files/sample.mp3').read_bytes()
    b = files['stereo-cbr']
    length = sum((b[6+i] & 127) << (7 * (3-i)) for i in range(4))
    bare = b[10+length:]
    files['bare'] = bare
    # Legal empty ID3v2.4 tag with footer and a different audio payload.
    m = files['mono-vbr']
    size = sum((m[6+i] & 127) << (7 * (3-i)) for i in range(4))
    tag = b'\x04\x00\x10\x00\x00\x00\x00'
    files['v24-footer'] = b'ID3' + tag + b'3DI' + tag + m[10+size:]
    return files


def verify(binary, root, independent=False):
    files = samples()
    disk = root / 'audio.raw'
    source = bytearray(b'\xac' * (1024 * 1024 - 15))
    offsets = {}
    for name in ['bare', *[k for k in files if k!='bare']]:
        b = files[name]
        offsets[len(source)] = name
        source.extend(b)
    disk.write_bytes(source)
    expected = {name: hashlib.sha256(b).hexdigest() for name,b in files.items()}
    def decode(path):
        return subprocess.check_output(['ffmpeg','-nostdin','-loglevel','error','-i',path,
                                        '-f','s16le','-acodec','pcm_s16le','-'])
    for mode, flags in [('session', []), ('index', ['--indexed'])]:
        scan = run(binary, 'scan', '--source', disk, '--mode', 'carve', '--extension', 'mp3',
                   '--work-dir', root/mode, '--json', *flags)
        found = run(binary, 'results', '--session', scan['session'], '--json')['candidates']
        check(len(found) == len(offsets) and {c['record'] for c in found} == offsets.keys(), found)
        for c in found:
            name = offsets[c['record']]
            check(c['supported'] and c['size'] == len(files[name]) and c['record_sha256']==expected[name], (name,c))
            row, = run(binary,'recover','--session',scan['session'],'--id',c['id'],
                       '--destination', root/(mode+'-out'),'--json')['results']
            output = Path(row['path'])
            check(output.read_bytes()==files[name] and row['sha256']==expected[name],row)
            if independent:
                original = root/(name+'.mp3')
                original.write_bytes(files[name])
                a, b = decode(original), decode(output)
                check(len(a)>0 and a==b,(name,'decoded PCM mismatch'))
        print(mode, len(found), 'MP3 files byte-exact', flush=True)
    b = files['stereo-cbr']
    x = b.index(b'Info')
    def edit(pos, replacement):
        return b[:pos] + replacement + b[pos+len(replacement):]
    bad = [('short',b[:-7]), ('size',edit(x+12,(len(b)+4000).to_bytes(4,'big'))),
           ('count',edit(x+8,(122).to_bytes(4,'big'))), ('flags',edit(x+4,b'\0\0\0\1')),
           ('magic',edit(x,b'NONE')), ('tag-size',edit(7,b'\x80')),
           ('bad-footer',files['v24-footer'][:10]+b'bad'+files['v24-footer'][13:])]
    for i,(name,data) in enumerate(bad):
        image = root/(name+'.bad')
        image.write_bytes(data)
        # Independent tagless audio may still be intact after a damaged ID3 tag.
        scan = run(binary,'scan','--source',image,'--mode','carve','--extension','mp3',
                   '--work-dir',root/f'bad-{i}','--json',expected=5)
        rows = run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(any(c['record']==0 and not c['supported'] for c in rows),(name,rows))
        if name not in ('tag-size','bad-footer'):
            check(all(not c['supported'] for c in rows),(name,rows))
    check(disk.read_bytes()==source,'source changed')
    return expected


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--independent',action='store_true')
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as d:
        verify(args.binary.resolve(),Path(d),args.independent)
