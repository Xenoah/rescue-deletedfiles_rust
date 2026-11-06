"""Index and JSON carving must retain the same evidence across real process loss."""
import argparse, hashlib, json, shutil, sqlite3, subprocess, tempfile, zipfile
from pathlib import Path
from e2e import check,run, candidate_evidence
from e2e_documents import make_variants,archive
from e2e_jpeg import build as jpeg_fixture
from make_carve_fixture import png


def main(binary,output=None):
    checks=0;exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-index-carve-') as temporary:
        root=Path(temporary);good,bad=make_variants()
        def compare(source,label,expected=0,extra=()):
            old=run(binary,'scan','--source',source,'--mode','carve','--work-dir',root/(label+'-json'),'--json',*extra,expected=expected)
            candidates=json.loads(Path(old['session']).read_text(encoding='utf-8'))['candidates']
            indexed=run(binary,'scan','--indexed','--source',source,'--mode','carve','--work-dir',root/(label+'-index'),'--json',*extra,expected=expected)
            page=run(binary,'results','--session',indexed['session'],'--limit','1000','--json')
            check(candidates==candidate_evidence(page),('candidate evidence differs',label))
            return Path(indexed['session']),candidates
        for (name,ext),data in good.items():
            source=root/(name+'.raw');source.write_bytes(data);index,rows=compare(source,name)
            top=next(c for c in rows if c['record']==0)
            result=run(binary,'recover','--session',index,'--id',top['id'],'--destination',root/(name+'-out'),'--json')['results'][0]
            check(result['status']=='completed' and Path(result['path']).read_bytes()==data,('indexed payload',name));checks+=1;exact+=1
        for name,data in bad.items():
            source=root/(name+'.raw');source.write_bytes(data);index,rows=compare(source,name,expected=5)
            check(not next(c for c in rows if c['record']==0)['supported'],('false valid',name));checks+=1
        source=root/'jpeg.raw';truth=jpeg_fixture(source);index,rows=compare(source,'jpeg')
        result=run(binary,'recover','--session',index,'--all','--archive',root/'jpeg.zip','--json')
        with zipfile.ZipFile(root/'jpeg.zip') as z:
            for row in json.loads(z.read('rescue-report.json'))['results']:check(z.read(row['archive_entry'])==truth[row['candidate_id']],'indexed JPEG/PNG bytes');exact+=1
        checks+=1
        for ext in ['pdf','zip','docx','xlsx','pptx','mp4','mov']:
            data=good[('real-'+ext,ext)];source=root/('boundary-'+ext+'.raw');source.write_bytes(bytes(1024*1024-3)+data+bytes(99))
            compare(source,'boundary-'+ext,extra=('--extension',ext));checks+=1
        source=root/'nested.raw';nested=archive([('padding.bin',bytes(2*1024*1024)),('inner.zip',archive([('a.txt',b'a'),('b.txt',b'b')],method=0))],method=0)
        with source.open('wb') as f:f.write(nested);f.seek(8*1024*1024-3);f.write(png());f.truncate(64*1024*1024)
        before=hashlib.sha256(source.read_bytes()).hexdigest()
        legacy=run(binary,'scan','--source',source,'--mode','carve','--work-dir',root/'nested-json','--json')
        expected=json.loads(Path(legacy['session']).read_text(encoding='utf-8'))['candidates']
        process=subprocess.Popen([str(binary),'scan','--indexed','--source',str(source),'--mode','carve','--work-dir',str(root/'crash'),'--progress','--json'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8')
        while True:
            line=process.stderr.readline();check(bool(line),'no checkpoint before exit');event=json.loads(line)
            if event['progress']['scanned_records']>=1024*1024:break
        path=Path(event['index']);process.kill();process.communicate(timeout=20)
        db=sqlite3.connect(path);saved=json.loads(db.execute('SELECT engine_state FROM checkpoint').fetchone()[0]);db.close()
        check(saved['attempts']>=1 and saved['validation_budget']<4*1024**3,'checkpoint reset validation accounting')
        # Use a copy to prove changed scan ranges are rejected without modifying
        # the valid original checkpoint used below.
        tampered=root/'tampered';shutil.copytree(path.parent,tampered);copy=tampered/path.name
        db=sqlite3.connect(copy);header=json.loads(db.execute('SELECT session FROM checkpoint').fetchone()[0]);header['search']['range_start']=17;db.execute('UPDATE checkpoint SET session=?',(json.dumps(header),));db.commit();db.close()
        failed=subprocess.run([str(binary),'resume','--session',str(copy),'--json'],capture_output=True,text=True,encoding='utf-8');check(failed.returncode!=0 and 'INVALID_REQUEST' in failed.stderr,'forged scan range resumed');checks+=1
        run(binary,'resume','--session',path,'--json')
        actual=candidate_evidence(run(binary,'results','--session',path,'--limit','1000','--json'));check(actual==expected,'ZIP suppression/candidate positions changed after crash');checks+=1
        db=sqlite3.connect(path);done=json.loads(db.execute('SELECT engine_state FROM checkpoint').fetchone()[0]);db.close()
        check(done['validation_budget']<=saved['validation_budget'] and done['attempts']>=saved['attempts'],'resume reset resource accounting');checks+=1
        run(binary,'recover','--session',path,'--all','--archive',root/'nested.zip','--json')
        with source.open('rb') as f,zipfile.ZipFile(root/'nested.zip') as z:
            for row in json.loads(z.read('rescue-report.json'))['results']:
                c=next(c for c in actual if c['id']==row['candidate_id']);f.seek(c['record']);check(z.read(row['archive_entry'])==f.read(c['size']),'resumed carving payload');exact+=1
        check(hashlib.sha256(source.read_bytes()).hexdigest()==before,'source changed');checks+=1
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':hashlib.sha256(Path(binary).read_bytes()).hexdigest(),'checks':checks,'files_byte_exact':exact,'source_unchanged':True}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(result));return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
