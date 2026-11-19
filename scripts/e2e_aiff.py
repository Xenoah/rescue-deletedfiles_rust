"""Carve FFmpeg AIFF/AIFF-C audio through ordinary and indexed sessions."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from e2e import check, run
from e2e_udif import digest

FIXTURES = Path(__file__).resolve().parents[1] / 'tests/aiff'


def chunk(name, data):
    return name + struct.pack('>I',len(data)) + data + bytes(len(data) % 2)


def form(kind, chunks):
    body = kind + b''.join(chunks)
    return b'FORM' + struct.pack('>I',len(body)) + body


def verify(binary, root, independent=False):
    manifest = json.loads((FIXTURES / 'manifest.json').read_text())
    files = {name: (FIXTURES / name).read_bytes() for name in manifest}
    for name, data in files.items():
        check(digest(data) == manifest[name]['sha256'], name)
    original = 'pcm_s16be.aiff'
    b = files[original]
    comm = b[20:38]
    ssnd = b[46:]
    # Nonzero SSND offset, reordered chunks and odd auxiliary payload padding.
    special = 'aligned.aiff'
    adjusted = struct.pack('>II',4,0) + bytes(4) + ssnd[8:]
    files[special] = form(b'AIFF',[chunk(b'ANNO',b'odd'), chunk(b'SSND',adjusted), chunk(b'COMM',comm)])
    disk = root / 'sound.raw'
    content = bytearray(b'\xa5' * (1024*1024-3))
    names = {}
    for name, data in files.items():
        names[len(content)] = name
        content.extend(data)
    disk.write_bytes(content)
    for label, flags in [('session',[]), ('indexed',['--indexed'])]:
        result = run(binary,'scan','--source',disk,'--mode','carve','--extension','aif',
                     '--work-dir',root/label,'--json',*flags)
        found = run(binary,'results','--session',result['session'],'--json')['candidates']
        check({r['record'] for r in found} == names.keys() and len(found)==len(names),found)
        for row in found:
            name = names[row['record']]
            check(row['size'] == len(files[name]) and row['record_sha256'] == digest(files[name])
                  and row['name'].endswith(Path(name).suffix),row)
            restored, = run(binary,'recover','--session',result['session'],'--id',row['id'],
                            '--destination',root/(label+'-out'),'--json')['results']
            path = Path(restored['path'])
            check(restored['sha256']==digest(files[name]) and path.read_bytes()==files[name],restored)
            if independent:
                reference = manifest[original if name == special else name]
                pcm = subprocess.check_output(['ffmpeg','-nostdin','-loglevel','error','-i',path,
                                               '-c:a','pcm_s32le','-f','s32le','-'])
                check(digest(pcm)==reference['pcm_s32_sha256'] and
                      len(pcm)==reference['frames']*reference['channels']*4,name)
        print(label,len(found),'AIFF / AIFF-C originals matched',flush=True)
    filtered=run(binary,'scan','--source',disk,'--mode','carve','--extension','afc',
                 '--work-dir',root/'alias','--json')
    rows=run(binary,'results','--session',filtered['session'],'--json')['candidates']
    check({names[r['record']] for r in rows}=={n for n in files if n.endswith('.aifc')},rows)
    def pcm(common=comm, sound=ssnd, extra=()):
        return form(b'AIFF', [chunk(b'COMM',common),chunk(b'SSND',sound),*extra])
    fc=files['pcm_f32be.aifc']
    bad = [b[:-1],pcm(comm[:2]+struct.pack('>I',9999)+comm[6:]),
           pcm(b'\0\0'+comm[2:]), pcm(comm[:8]+bytes(10)),
           pcm(comm[:6]+b'\0\0'+comm[8:]),pcm(sound=b'\xff'*4+ssnd[4:]),
           pcm(sound=ssnd+b'garbage'),pcm(extra=[chunk(b'COMM',comm)]),pcm(extra=[chunk(b'SSND',ssnd)]),
           form(b'AIFF',[chunk(b'COMM',comm)]),form(b'AIFF',[chunk(b'SSND',ssnd)]),
           fc.replace(b'fl32',b'FAKE'),fc.replace(b'FVER',b'JUNK'),fc.replace(bytes.fromhex('a2805140'),bytes(4)),
           fc.replace(b'fl32\0\0',b'fl32\x10\0')]
    for i, data in enumerate(bad):
        path=root/f'bad-{i}.img';path.write_bytes(data)
        result=run(binary,'scan','--source',path,'--mode','carve','--work-dir',root/f'bad-{i}','--extension','aif','--json',expected=5)
        rows=run(binary,'results','--session',result['session'],'--json')['candidates']
        check(rows and all(not r['supported'] for r in rows),(i,rows))
    check(disk.read_bytes()==content,'source changed')
    print(len(bad),'malformed audio structures refused',flush=True)
    return {name:digest(b) for name,b in files.items()}


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--independent',action='store_true')
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        verify(args.binary.resolve(),Path(temp),args.independent)
