"""Use xorriso to independently author two ISO sessions and remove files in the second session."""
import argparse
import hashlib
from pathlib import Path
import subprocess
import tempfile
from e2e import check, run

CORPUS = Path(__file__).resolve().parents[1] / 'tests/corpus/files'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def xorriso(*args):
    return subprocess.run(['xorriso','-no_rc','-abort_on','SORRY','-report_about','SORRY',*map(str,args)],
                          check=True,capture_output=True,timeout=90).stdout


def verify(binary, root):
    before = root / 'before'
    before.mkdir()
    lost = before / 'Removed'
    lost.mkdir()
    records = {'ordinary.txt':'sample.txt','inventory.csv':'sample.csv','drawing.png':'sample.png',
               'document.zip':'sample.zip','audio.flac':'sample.flac',"日本語の'記録.dat":'sample.pdf',
               'line\nbreak.xml':'sample.xml','a\\b.rtf':'sample.rtf','with space.jpg':'sample.jpg'}
    originals = {}
    for name, source in records.items():
        data = (CORPUS / source).read_bytes()
        (lost / name).write_bytes(data)
        originals['/Removed/' + name] = data
    for name, data in [('vendor.unknown', bytes(range(256))*130+b'end'),('empty.bin', b'')]:
        (lost/name).write_bytes(data)
        originals['/Removed/'+name] = data
    (before/'survivor.dat').write_bytes(b'live')
    (before/'became-directory').write_bytes(b'original regular file')
    image = root / 'retained.iso'
    xorriso('-outdev','stdio:'+str(image),'-volid','HISTORY','-map',before,'/',
            '-set_filter','--zisofs','/Removed/vendor.unknown','--','-commit','-end')
    stream = xorriso('-indev',image,'-find','/Removed/vendor.unknown','-exec','show_stream','--')
    check(b'--zisofs-decode' in stream,'stored zisofs stream')
    currentfile = root/'new';currentfile.write_bytes(b'created later')
    xorriso('-dev','stdio:'+str(image),'-rm_r','/Removed','/became-directory','--',
            '-mkdir','/became-directory','--','-map',currentfile,'/added','-commit','-end')
    original_image = sha(image.read_bytes())
    historical = root / 'reference'
    xorriso('-load','session','1','-indev',image,'-osirrox','on','-extract','/Removed',historical)
    for name,data in originals.items():
        check((historical/name.removeprefix('/Removed/')).read_bytes()==data,name)
    current = xorriso('-indev',image,'-ls','/').decode()
    check("'Removed'" not in current and "'survivor.dat'" in current and "'added'" in current,current)
    cmd=['iso-sessions','--source',image,'--json']
    table=run(binary,*cmd)
    check(table['current_session']==2 and [v['number'] for v in table['sessions']]==[1,2],table)
    history=run(binary,*cmd,'--session','1')
    check({x['original_path']:x['size'] for x in history['candidates']} ==
          {name:len(data) for name,data in originals.items()},history)
    destination=root/'output'
    results=run(binary,*cmd,'--session','1','--all','--destination',destination)['results']
    checked={}
    for row in results:
        name=row['original_path'];p=Path(row['path']);actual=p.read_bytes()
        check(p.parent==destination and actual==originals[name] and sha(actual)==row['sha256'],name)
        checked[name]=sha(actual)
    check(checked.keys()==originals.keys(),checked)
    pick="/Removed/日本語の'記録.dat"
    single,=run(binary,*cmd,'--session','1','--path',pick,'--destination',root/'one')['results']
    check(single['original_path']==pick and Path(single['path']).read_bytes()==originals[pick],single)
    refused=run(binary,*cmd,'--session','1','--all','--destination',destination,expected=5)
    check(len(refused['results'])==len(checked) and all('error' in r for r in refused['results']),refused)
    for args in [('--session','0'),('--session','2'),('--session','3'),('--session','1','--path','/added','--destination',root/'bad')]:
        run(binary,*cmd,*args,expected=2)
    check(not (root/'bad').exists() and sha(image.read_bytes())==original_image,'source and destinations')
    print(len(checked),'historical files restored with exact original bytes and SHA-256; live paths and replacement directories excluded',flush=True)
    return checked


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',required=True,type=Path)
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as directory:
        verify(args.binary.resolve(),Path(directory))
