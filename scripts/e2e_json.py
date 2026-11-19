"""Recover the encoded bytes of bounded UTF-8 JSON objects and arrays."""
import argparse
import codecs
import hashlib
import json
from pathlib import Path
import tempfile
from e2e import check, run

ROOT=Path(__file__).resolve().parents[1]


def fixtures():
    return {
        'unicode':json.dumps({'復元':'文字🙂','line':'one\ntwo'},ensure_ascii=False).encode(),
        'nested':json.dumps({'data':[{'id':1,'child':{'x':'[1,2]'}},{'id':2}]}).encode(),
        'javascript':b'{"first":"escape\\"quoted","text":"} [ {","slash":"\\\\","number":-1.25e+05}',
        'pretty':json.dumps({'doc':{'page':1},'enabled':True,'empty':None},indent=2).encode(),
        'array-of-objects':b'[  {"key": 1}, {"key": 2}]',
        'surrogates':br'{"name":"\u65e5\u672c\ud83d\ude00"}',
        'bom':codecs.BOM_UTF8+b' \n{"value":[1,2,3]}',
        'long':json.dumps({'value':'x'*70001,'inner':{'finish':'ok'}}).encode(),
        'corpus':(ROOT/'tests/corpus/files/sample.json').read_bytes(),
        'primitives':b'[ -1, 0.25e2, "x",true, false, null ]',
        'one-array':b'[[1],[{"a":2}]]',
        'empty':b'{}',
    }


def verify(binary,root):
    files=fixtures()
    for b in files.values():json.loads(b)
    disk=root/'records.raw'
    encoded=bytearray(b'\xff'*(1024*1024-2))
    offsets={}
    for name,b in files.items():
        offsets[len(encoded)]=name
        encoded.extend(b+b' \r\n\xf0\x8a[t\x90t[fX[nx')
    disk.write_bytes(encoded)
    for engine,flags in [('session',[]),('index',['--indexed'])]:
        scan=run(binary,'scan','--source',disk,'--mode','carve','--extension','json',
                 '--work-dir',root/engine,'--json',*flags)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check({r['record'] for r in rows}==offsets.keys() and len(rows)==len(files),[(r['record'],r['name']) for r in rows])
        for row in rows:
            name=offsets[row['record']];original=files[name]
            check(row['supported'] and row['size']==len(original) and row['extent_evidence']=='file_structure',name)
            restored,=run(binary,'recover','--session',scan['session'],'--id',row['id'],'--destination',root/(engine+'-files'),'--json')['results']
            b=Path(restored['path']).read_bytes()
            check(b==original and hashlib.sha256(b).hexdigest()==restored['sha256'] and json.loads(b)==json.loads(original),name)
        print(engine,len(rows),'JSON values byte-exact',flush=True)
    # The default scan requires a distinctive key/colon prologue. Embedded objects in
    # a successfully validated JSON value do not appear a second time.
    simple=root/'strong.raw';simple.write_bytes(files['nested']+b'\xff'+files['primitives']+b'\xf0'+files['empty'])
    result=run(binary,'scan','--source',simple,'--mode','carve','--work-dir',root/'default','--json')
    rows=run(binary,'results','--session',result['session'],'--json')['candidates']
    check([r['record'] for r in rows if r['name'].endswith('.json')]==[0],rows)
    negatives=[b'{"k":1,}',b'{"k":[1,2}',b'{"z":true',b'{"k":"bad\x01"}',b'{"k":"bad\xff"}',
               br'{"k":"\uD800"}',br'{"k":"\x01"}',b'{"k":01}',b'{"key":NaN}',b'{"s":/3}',
               b'{"s":'+b'['*124+b'1'+b']'*124+b'}',b'{"a":1 /* comment */}']
    for i,b in enumerate(negatives):
        source=root/f'bad-{i}.json';source.write_bytes(b)
        scan=run(binary,'scan','--source',source,'--extension','json','--mode','carve','--work-dir',root/f'bad-{i}','--json',expected=5)
        rows=run(binary,'results','--session',scan['session'],'--json')['candidates']
        check(any(r['record']==0 and not r['supported'] for r in rows),i)
    check(disk.read_bytes()==encoded,'source changed')
    print(len(negatives),'malformed or unsupported outer values refused',flush=True)
    return {k:hashlib.sha256(v).hexdigest() for k,v in files.items()}


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True)
    with tempfile.TemporaryDirectory() as t:verify(p.parse_args().binary.resolve(),Path(t))
