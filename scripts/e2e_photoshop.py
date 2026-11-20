"""Recover layered Photoshop documents and verify the composite and layer samples with psd-tools."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import tempfile
import zlib

from e2e import check, run
from e2e_html import make_deleted_fat
from e2e_imaging import mark_gap
from e2e_netpbm import make_deleted_ext4

GOLD = Path(__file__).resolve().parents[1]/'tests/photoshop'
FILTER = ['--extension','psd','--extension','psb']


def sha(data):
    return hashlib.sha256(data).hexdigest()


def decode(path):
    from psd_tools import PSDImage
    doc = PSDImage.open(path)
    header = doc._record.header
    item = doc._record.image_data
    layers = []
    for layer in doc.descendants():
        samples = []
        for entry, channel, size in zip(layer._record.channel_info, layer._channels, layer._record.channel_sizes, strict=True):
            if 0 in size:
                check(not channel.data,(layer.name,size))
                sample = b''
            else:
                sample = channel.get_data(*size,header.depth,header.version)
            samples.append([int(entry.id),*size,sha(sample)])
        layers.append([layer.name,list(layer.bbox),samples])
    return dict(version=header.version, dimensions=[header.width,header.height], depth=header.depth,
                channels=header.channels, mode=int(header.color_mode), compression=int(item.compression),
                composite_sha256=sha(item.get_data(header,split=False)), layers=layers)


def recover(binary, disk, folder, files, manifest, native):
    digests = {sha(b):n for n,b in files.items()}
    before = sha(disk.read_bytes())
    for label, flags in [('ordinary',[]),('indexed',['--indexed'])]:
        scan = run(binary,'scan','--mode','carve','--source',disk,'--work-dir',folder/label,
                   *FILTER,*flags,'--json',timeout=180)
        found = run(binary,'results','--session',scan['session'],'--limit',100,'--json')['candidates']
        check(len(found)==len(files) and {r['record_sha256'] for r in found}==digests.keys(), found)
        check(all(r['supported'] and r['extent_evidence']=='file_structure'
                  and r['name'].endswith(Path(digests[r['record_sha256']]).suffix) for r in found),found)
        rows = run(binary,'recover','--session',scan['session'],'--all','--destination',folder/(label+'-files'),
                   '--json',timeout=180)['results']
        check(len(rows)==len(files),rows)
        for row in rows:
            name=digests[row['sha256']]
            path=Path(row['path'])
            check(row['status']=='completed' and path.read_bytes()==files[name] and sha(path.read_bytes())==manifest[name]['sha256'],row)
            if native:
                check(decode(path)==manifest[name]['decoded'],name)
    check(sha(disk.read_bytes())==before,'source changed')
    print(disk.name,len(files),'Photoshop originals match in ordinary and indexed recovery',flush=True)


def composite(data):
    wide=8 if data[5]==2 else 4
    pos=26
    for size in (4,4,wide):
        pos+=size+int.from_bytes(data[pos:pos+size],'big')
    return pos


def layer_channels(data, tagged=False):
    """Locate recorded channel headers and payloads for targeted damage to an intact document."""
    wide = 8 if data[5] == 2 else 4
    start = 26
    for _ in range(2):
        start += 4 + int.from_bytes(data[start:start+4], 'big')
    pos = data.index(b'8BIMLr16') + 12 if tagged else start + wide * 2
    count = abs(int.from_bytes(data[pos:pos+2], 'big', signed=True))
    pos += 2
    entries = []
    for _ in range(count):
        pos += 16
        channels = int.from_bytes(data[pos:pos+2], 'big')
        pos += 2
        for _ in range(channels):
            channel = int.from_bytes(data[pos:pos+2], 'big', signed=True)
            size = int.from_bytes(data[pos+2:pos+2+wide], 'big')
            entries.append((channel, pos, size))
            pos += 2 + wide
        pos += 12
        pos += 4 + int.from_bytes(data[pos:pos+4], 'big')
    result = []
    for channel, header, size in entries:
        result.append((channel, header, pos, pos+size))
        pos += size
    return result


def damaged(binary, folder, files):
    raw=files['rgb-raw.psd']
    zipped=files['rgb-zip.psd']
    packed=files['layers-rle.psb']
    def edit(data,pos,what):
        return data[:pos]+what+data[pos+len(what):]
    p=composite(zipped)
    q=composite(packed)
    layer_section=26
    for _ in range(2):
        layer_section+=4+int.from_bytes(packed[layer_section:layer_section+4],'big')
    layer=raw.index(b'8BIM',raw.index(b'8BIM')+4)
    bad = {
        'reserved': edit(raw,6,b'!'), 'zero-width':edit(raw,18,bytes(4)), 'channel-count':edit(raw,12,b'\0\x02'),
        'bad-depth':edit(raw,22,b'\0\x03'), 'unknown-mode':edit(raw,24,b'\0\x06'),
        'oversized-rgb':edit(raw,14,struct.pack('>II',30000,30000)), 'color-length':edit(raw,26,b'\xff'*4),
        'resource-signature':edit(raw,raw.index(b'8BIM'),b'8BAA'), 'layer-blend':edit(raw,layer,b'7BIM'),
        'unknown-compression':edit(raw,composite(raw),b'\0\x04'), 'raw-truncated':raw[:-1],
        'rle-truncated':packed[:-1], 'rle-row-too-long':edit(packed,q+2,b'\xff'*4),
        'rle-empty-row':edit(packed,q+2,bytes(4)), 'zip-truncated':zipped[:-2],
        'zip-checksum':zipped[:-1]+bytes([zipped[-1]^255]),
        'zip-short-output':zipped[:p+2]+zlib.compress(b'abc'),
        'zip-long-output':zipped[:p+2]+zlib.compress(bytes(31*17*3+1)),
        'psb-section-overflow':edit(packed,layer_section,b'\xff'*8),
    }
    for label, filename, channel, mutation in [
        ('layer-id', 'rgb-raw.psd', 0, 'duplicate'),
        ('layer-method', 'deep-layers-raw.psb', 0, 'method'),
        ('layer-raw-length', 'deep-layers-raw.psb', 0, 'size'),
        ('layer-rle-row', 'gimp-layers.psd', 0, 'row'),
        ('layer-psb-row', 'layers-rle.psb', 0, 'row'),
        ('layer-zip-checksum', 'gray-layers-zip.psd', 0, 'checksum'),
        ('layer-psb-zip-checksum', 'rgb-layers-zip.psb', 0, 'checksum'),
        ('mask-zip-checksum', 'mask-prediction.psd', -2, 'checksum'),
        ('tagged-layer-checksum', 'deep-tag-prediction.psd', 0, 'checksum'),
    ]:
        data = files[filename]
        _, header, start, end = next(c for c in layer_channels(data,filename.startswith('deep-tag')) if c[0]==channel)
        wide = 8 if filename.endswith('.psb') else 4
        if mutation == 'duplicate':
            bad[label] = edit(data,header,b'\0\1')
        elif mutation == 'method':
            bad[label] = edit(data,start,b'\0\4')
        elif mutation == 'size':
            bad[label] = edit(data,header+2,(end-start-1).to_bytes(wide,'big'))
        elif mutation == 'row':
            bad[label] = edit(data,start+2,b'\xff'*(wide//2))
        else:
            bad[label] = edit(data,end-1,bytes([data[end-1]^0xff]))
    for name,data in bad.items():
        path=folder/(name+'.raw')
        path.write_bytes(data)
        found=run(binary,'scan','--source',path,'--mode','carve',*FILTER,'--work-dir',folder/name,'--json',expected=5)
        rows=run(binary,'results','--session',found['session'],'--json')['candidates']
        check(rows and all(not r['supported'] for r in rows),(name,rows))
    path=folder/'gapped.raw'
    path.write_bytes(b'!'*(4096-len(zipped))+zipped+b'!'*(8192-70)+files['rgb-prediction.psb']+bytes(4096))
    capture=run(binary,'image','--source',path,'--destination',folder/'acquired','--block-size',4096,'--json')
    image=Path(capture['image'])
    mark_gap(image,4096)
    mark_gap(image,12288)
    found=run(binary,'scan','--source',image,'--mode','carve',*FILTER,'--work-dir',folder/'gapped','--json',expected=5)
    rows=run(binary,'results','--session',found['session'],'--json')['candidates']
    check({r['record_sha256'] for r in rows if r['supported']}=={sha(zipped)},rows)
    check(any(not r['supported'] and r['record']==12288-70 for r in rows),rows)
    return sorted(bad)


def verify(binary, root, native=False):
    manifest=json.loads((GOLD/'manifest.json').read_text(encoding='utf-8'))
    files={name:(GOLD/name).read_bytes() for name in manifest}
    hashes={name:sha(data) for name,data in files.items()}
    check(all(item['bytes']==len(files[n]) and item['sha256']==hashes[n] for n,item in manifest.items()),manifest)
    if native:
        for n in manifest:
            check(decode(GOLD/n)==manifest[n]['decoded'],n)
        for kind,create in [('fat32',make_deleted_fat),('ext4',make_deleted_ext4)]:
            folder=root/kind
            folder.mkdir()
            disk=create(folder,files)
            recover(binary,disk,folder,files,manifest,True)
        return {'originals':hashes,'filesystems':['fat32','ext4'],
                'decoded':{n:item['decoded'] for n,item in manifest.items()}}
    disk=root/'adjacent.raw'
    disk.write_bytes(b'!'*((1<<20)-3)+b''.join(files.values()))
    recover(binary,disk,root,files,manifest,False)
    for ext in ('psd','psb'):
        found=run(binary,'scan','--source',disk,'--mode','carve','--extension',ext,'--work-dir',root/ext,'--json')
        rows=run(binary,'results','--session',found['session'],'--limit',100,'--json')['candidates']
        check({r['record_sha256'] for r in rows}=={h for n,h in hashes.items() if n.endswith('.'+ext)},ext)
    return {'originals':hashes,'refused':damaged(binary,root,files)}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--native',action='store_true')
    p.add_argument('--output',type=Path)
    a=p.parse_args()
    with tempfile.TemporaryDirectory() as scratch:
        result=verify(a.binary.resolve(),Path(scratch),a.native)
    if a.output:
        a.output.write_text(json.dumps(result,indent=2,ensure_ascii=False,sort_keys=True)+'\n',encoding='utf-8')
