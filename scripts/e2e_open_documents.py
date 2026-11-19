"""Classify real ODF and EPUB packages and preserve generic ZIP recovery for ambiguous archives."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import tempfile
import zipfile
from e2e import check, run

FIXTURES=Path(__file__).resolve().parents[1]/'tests'/'open-documents'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def samples():
    manifest=json.loads((FIXTURES/'manifest.json').read_text())['files']
    files={n:(FIXTURES/n).read_bytes() for n in manifest}
    check(all(sha(b)==manifest[n]['sha256'] for n,b in files.items()),'fixture sha256')
    return files


def rewrite(payload, changes=None, skip=(), first=True, compressed=False, duplicate=False):
    output=io.BytesIO()
    with zipfile.ZipFile(io.BytesIO(payload)) as original, zipfile.ZipFile(output,'w') as target:
        entries=original.namelist()
        if not first:entries=entries[1:]+entries[:1]
        for name in entries:
            if name in skip:continue
            data=original.read(name)
            if changes and name in changes:data=changes[name](data)
            method=zipfile.ZIP_DEFLATED if compressed or name!='mimetype' else zipfile.ZIP_STORED
            target.writestr(name,data,compress_type=method)
        if duplicate:
            import warnings
            with warnings.catch_warnings():
                warnings.simplefilter('ignore',UserWarning)
                target.writestr('mimetype',original.read('mimetype'))
    return output.getvalue()


def recover(binary,path,root,files,extensions,indexed):
    flags=[v for ext in sorted(extensions) for v in ('--extension',ext)]
    scan=run(binary,'scan','--source',path,'--mode','carve','--work-dir',root/'sessions','--json',
             *flags,*(['--indexed'] if indexed else []))
    rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
    expected={sha(b):n for n,b in files.items()}
    check(len(rows)==len(files) and all(r['supported'] for r in rows),[(r['name'],r['supported']) for r in rows])
    for row in rows:
        name=expected[row['record_sha256']]
        check(Path(row['name']).suffix==Path(name).suffix and row['extent_evidence']=='zip_crc_validated',(name,row))
        result,=run(binary,'recover','--session',scan['session'],'--id',row['id'],'--destination',root/'recovered','--json')['results']
        restored=Path(result['path']).read_bytes()
        check(restored==files[name] and result['sha256']==sha(restored),name)
        with zipfile.ZipFile(io.BytesIO(restored)) as package:check(package.testzip() is None,name)
    return len(rows)


def verify(binary,root,previous=None):
    files=samples();disk=root/'documents.raw'
    original=bytes(1024*1024-2)+b'\xe7\x96'.join(files.values())
    disk.write_bytes(original)
    exts={Path(n).suffix[1:] for n in files}
    for indexed in [False,True]:
        n=recover(binary,disk,root/str(indexed),files,exts,indexed)
        print('index' if indexed else 'session',n,'typed documents and ebooks byte-exact',flush=True)
    # Archival ZIP filters continue to include packages that used to be named .zip.
    book={'ebooklib.epub':files['ebooklib.epub']}
    one=root/'one.raw';one.write_bytes(next(iter(book.values())))
    recover(binary,one,root/'zip-filter',book,{'zip'},False)
    odf=files['odfpy.odt'];epub=files['ebooklib.epub']
    with zipfile.ZipFile(io.BytesIO(epub)) as z:
        opf=next(n for n in z.namelist() if n.endswith('.opf'))
    m='META-INF/manifest.xml';c='META-INF/container.xml'
    bad=[rewrite(odf,first=False),rewrite(odf,compressed=True),rewrite(odf,duplicate=True),
         rewrite(odf,skip=['content.xml']),rewrite(odf,{m:lambda b:b.replace(b'full-path="/"',b'full-path="x"')}),
         rewrite(odf,{'mimetype':lambda _:b'application/vnd.oasis.opendocument.spreadsheet'}),
         rewrite(odf,{m:lambda _:b'<bad/>'}),rewrite(epub,skip=[c]),rewrite(epub,skip=[opf]),
         rewrite(epub,{c:lambda b:b.replace(opf.encode(),b'../outside.opf')}),
         rewrite(epub,{opf:lambda _:b'<package/>'}),rewrite(epub,{c:lambda _:b'<bad/>'})]
    for i,encoded in enumerate(bad):
        d=root/f'bad-{i}';d.mkdir();p=d/'data.zip';p.write_bytes(encoded)
        scan=run(binary,'scan','--source',p,'--mode','carve','--extension','zip','--work-dir',d/'sessions','--json')
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(len(rows)==1 and rows[0]['name'].endswith('.zip') and rows[0]['record_sha256']==sha(encoded),(i,rows))
    if previous:
        for index in (False,True):
            d=root/('old-'+str(index))
            scan=run(previous,'scan','--source',one,'--mode','carve','--extension','zip','--work-dir',d/'sessions','--json',*(['--indexed'] if index else []))
            row,=run(previous,'results','--session',scan['session'],'--json')['candidates']
            check(row['name'].endswith('.zip'),row)
            restored,=run(binary,'recover','--session',scan['session'],'--id',row['id'],'--destination',d/'out','--json')['results']
            check(Path(restored['path']).read_bytes()==epub and restored['sha256']==sha(epub),restored)
        print('Recovered both generic ZIP sessions written by the previous binary',flush=True)
    check(disk.read_bytes()==original,'image changed')
    print(len(bad),'ambiguous or malformed package descriptions kept as ZIP',flush=True)
    return {name:sha(b) for name,b in files.items()}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--previous',type=Path)
    args=p.parse_args()
    with tempfile.TemporaryDirectory() as t:
        verify(args.binary.resolve(),Path(t),args.previous.resolve() if args.previous else None)
