"""Use independent extent maps and original SHA-256 to restore files of any content type."""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from e2e import check, run
from e2e_imaging import mark_gap

CORPUS = Path(__file__).resolve().parents[1] / 'tests/corpus/files'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def manifest(original, extents):
    return {'schema_version': 1, 'sha256': sha(original), 'extents': extents}


def restore(binary, source, layout, output, expected=0):
    return run(binary, 'recover-extents', '--source', source, '--layout', layout,
               '--output', output, '--json', expected=expected)


def verify(binary, root):
    files = {ext: (CORPUS / ('sample.' + ext)).read_bytes()
             for ext in ('txt', 'csv', 'aac', 'html', 'tar', 'pdf', 'sqlite', 'png')}
    files['unknown'] = b'\x14\x96private\0payload' * 731
    files['sparse'] = b'head' + bytes(270031) + b'tail' + bytes(1024)
    files['empty'] = b''
    image = root / 'storage.bin'
    disk = bytearray(b'\xb4' * 4096)
    plans = {}

    def append(chunk):
        offset = len(disk)
        disk.extend(chunk)
        disk.extend(b'other data\xa7' * 19)
        return {'offset': offset, 'length': len(chunk)}

    for name, content in files.items():
        if name == 'empty':
            layout = []
        elif name == 'sparse':
            layout = [append(b'head'), {'zero_bytes':270031}, append(b'tail'), {'zero_bytes':1024}]
        else:
            a, b = len(content)//3, len(content)*2//3
            tail = append(content[b:])
            first = append(content[:a])
            middle = append(content[a:b])
            layout = [first, middle, tail]
        plans[name] = manifest(content, layout)
    image.write_bytes(disk)
    plan = root / 'layout.json'
    for name, original in files.items():
        plan.write_text(json.dumps(plans[name]))
        out = root / ('restored.' + name)
        report = restore(binary, image, plan, out)
        check(out.read_bytes() == original and report['file']['sha256'] == sha(original)
              and report['original_sha256_verified'] and report['file']['bytes'] == len(original), name)
        restore(binary, image, plan, out, expected=2)
        check(out.read_bytes() == original, name + ' collision')
    print(len(files), 'known-layout files match original SHA-256, including unknown and sparse content', flush=True)
    repeated = files['csv'] + files['csv']
    repeated_plan = manifest(repeated, plans['csv']['extents']*2)
    plan.write_text(json.dumps(repeated_plan))
    restore(binary, image, plan, root/'repeated.csv')
    check((root/'repeated.csv').read_bytes() == repeated, 'repeated regions')

    failures = [
        (dict(plans['txt'], sha256='0'*64), 4),
        (dict(plans['txt'], sha256='x'*64), 2),
        (dict(plans['txt'], schema_version=9), 2),
        (dict(plans['txt'], extents=[{'offset':len(disk)-1,'length':2}]), 4),
        (dict(plans['txt'], extents=[{'offset':2**64-1,'length':2}]), 4),
        (dict(plans['txt'], extents=[{'offset':0,'length':0}]), 2),
        (dict(plans['txt'], extents=[{'zero_bytes':0}]), 2),
        (dict(plans['txt'], extents=[{'zero_bytes':2**64-1}]), 4),
        (dict(plans['txt'], extents=[{'offset':0,'length':7,'zero_bytes':7}]), 2),
        (dict(plans['txt'], path='/unused'),2),
        (dict(plans['txt'], extents=[{'offset':0,'length':1}]*65537),2),
    ]
    dest = root/'reject';dest.mkdir()
    for n,(bad,code) in enumerate(failures):
        plan.write_text(json.dumps(bad))
        restore(binary,image,plan,dest/'file',expected=code)
        check(not any(dest.iterdir()), n)
    plan.write_text(json.dumps(plans['txt']))
    restore(binary,image,plan,Path(str(image)+'.rmap'),expected=4)
    check(not Path(str(image)+'.rmap').exists(),'reserved source read map')
    plan.write_text(json.dumps(dict(plans['csv'],sha256=sha(files['csv']).upper())))
    args=[str(binary),'recover-extents','--source',str(image),'--layout',str(plan),'--output',str(root/'race.csv'),'--json']
    with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
        codes=list(pool.map(lambda _:subprocess.run(args,capture_output=True).returncode,range(5)))
    check(codes.count(0)==1 and (root/'race.csv').read_bytes()==files['csv'],codes)
    from e2e_raid import encode, save
    raid_config, _ = encode(root/'members', bytes(disk), 'raid0', n=2)
    array = save(root, raid_config)
    plan.write_text(json.dumps(plans['sqlite']))
    raid_file = root/'from-raid.sqlite'
    restore(binary, array, plan, raid_file)
    check(raid_file.read_bytes() == files['sqlite'],'RAID extents')
    member = Path(raid_config['members'][0]['path'])
    restore(binary, array, plan, Path(str(member)+'.rmap'), expected=4)
    # A source range containing real zeros remains unreadable if the acquisition
    # marks that region as missing, even when substituting zeros would match the hash.
    pad=root/'zeros.raw';pad.write_bytes(bytes(16384))
    aq=run(binary,'image','--source',pad,'--destination',root/'acquired','--block-size','512','--json')
    acquired=Path(aq['image']);mark_gap(acquired,1024)
    for at,expected in [(1024,4),(2048,0)]:
        plan.write_text(json.dumps(manifest(bytes(512),[{'offset':at,'length':512}])))
        out=root/f'range-{at}'
        result=restore(binary,acquired,plan,out,expected=expected)
        if expected:
            check('MISSING_RANGES' in result and not out.exists(),result)
        else:
            check(out.read_bytes()==bytes(512),result)
    check(image.read_bytes()==disk, 'source changed')
    print('Rejected malformed layouts, failed hashes and a marked missing sector; concurrent output retained exactly one winner',flush=True)
    return {n:sha(b) for n,b in files.items()}


def native(binary,root):
    # debugfs bmap is an independent source of the pre-deletion physical layout.
    fs=root/'ext4.raw'
    with fs.open('wb') as stream:stream.truncate(16<<20)
    subprocess.run(['mke2fs','-q','-F','-t','ext4','-b','1024',fs],check=True)
    def debug(command,write=False):
        r=subprocess.run(['debugfs',*(['-w'] if write else []),'-R',command,fs],cwd=CORPUS,
                         check=True,capture_output=True,text=True)
        return r.stdout.strip()
    samples=['sample.'+ext for ext in ('csv','aac','txt','html','tar','pdf','sqlite','png')]
    layouts={}
    for name in samples:
        original=(CORPUS/name).read_bytes()
        debug(f'write {name} /{name}',True)
        extents=[]
        for pos in range(0,len(original),1024):
            block=int(debug(f'bmap /{name} {pos//1024}'));length=min(1024,len(original)-pos)
            extents.append({'offset':block*1024,'length':length} if block else {'zero_bytes':length})
        layouts[name]=manifest(original,extents)
    for name in samples:debug('rm /'+name,True)
    listing=debug('ls -p /')
    check(all('/'+n+'/' not in listing for n in samples),'actual unlink')
    before=sha(fs.read_bytes())
    for n,layout in layouts.items():
        plan=root/(n+'.json');plan.write_text(json.dumps(layout))
        original=(CORPUS/n).read_bytes();out=root/n
        result=restore(binary,fs,plan,out)
        check(out.read_bytes()==original and result['file']['sha256']==sha(original),n)
    check(sha(fs.read_bytes())==before,'filesystem image changed')
    print('ext4: recovered eight unlinked files using pre-deletion e2fsprogs block maps and original digests',flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',required=True,type=Path)
    parser.add_argument('--native',action='store_true')
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as t:
        (native if args.native else verify)(args.binary.resolve(),Path(t))
