"""Export real recovery JSON, preserving evidence and escaping hostile file names."""
import argparse,csv,hashlib,io,json,subprocess,tempfile,zipfile
from html.parser import HTMLParser
from pathlib import Path
from e2e import check,run
from make_fixture import build
from make_fat_fixture import file_hash

class Document(HTMLParser):
    def __init__(self):super().__init__(convert_charrefs=True);self.tags=[];self.pre=[];self.current=None;self.text=[]
    def handle_starttag(self,tag,attrs):
        self.tags.append((tag,dict(attrs)))
        if tag=='pre':self.current=''
    def handle_endtag(self,tag):
        if tag=='pre':self.pre.append(self.current);self.current=None
    def handle_data(self,data):
        self.text.append(data)
        if self.current is not None:self.current+=data

def main(binary,output=None):
    checks=[];exact=0
    with tempfile.TemporaryDirectory(prefix='rescue-reports-') as temporary:
        root=Path(temporary);source=root/'source.raw';truth=build(source);before=file_hash(source)
        scan=run(binary,'scan','--source',source,'--work-dir',root/'work','--json')
        recovered=run(binary,'recover','--session',scan['session'],'--all','--destination',root/'restored','--json')
        report=Path(recovered['report']);original=json.loads(report.read_text(encoding='utf-8'));report_before=file_hash(report)
        for row in original['results']:
            check(row['status']=='completed' and Path(row['path']).read_bytes()==bytes.fromhex(truth[row['candidate_id']]['hex']),row);exact+=1
        def export(data,prefix):
            input_path=root/(prefix+'.json');input_path.write_text(json.dumps(data,ensure_ascii=False),encoding='utf-8')
            for format in ['html','csv']:
                out=root/(prefix+'.'+format);result=run(binary,'export-report','--input',input_path,'--output',out,'--format',format,'--json')
                check(result['results']==len(data['results']),'export result count')
                if format=='html':
                    document=Document();document.feed(out.read_text(encoding='utf-8'))
                    check(not any(tag in ['script','img','iframe','svg','object','a'] for tag,attrs in document.tags),'active HTML content')
                    check(any(tag=='meta' and attrs.get('http-equiv')=='Content-Security-Policy' for tag,attrs in document.tags),'missing offline CSP')
                    check([json.loads(p) for p in document.pre[:-1]]==data['results'],'HTML changed result evidence')
                    check(json.loads(document.pre[-1])=={k:v for k,v in data.items() if k!='results'},'HTML changed source metadata')
                    check(sum(tag=='tr' for tag,attrs in document.tags)==len(data['results'])+1,'HTML rows')
                else:
                    check(out.read_bytes().startswith(b'\xef\xbb\xbf'),'CSV missing UTF-8 BOM')
                    rows=list(csv.DictReader(io.StringIO(out.read_bytes().decode('utf-8-sig'))))
                    check(len(rows)==len(data['results']),'CSV rows/newlines')
                    for row,expected in zip(rows,data['results']):
                        for key in ['candidate_id','status','path','archive_entry','bytes_written','sha256','extent_evidence','allocation_status','warnings','error','error_details','naming','missing_ranges','raid_reads']:
                            value=expected.get(key);actual=row[key]
                            if isinstance(value,(dict,list)):check(json.loads(actual)==value,('CSV structured field',key));continue
                            text='' if value is None else str(value)
                            dangerous=text.startswith(('\t','\r','\n')) or text.lstrip().startswith(('=','+','-','@'))
                            check(actual==("'" if dangerous else '')+text,('CSV field changed',key,actual,text))
                        check(json.loads(row['result_json'])==expected,'CSV full result');check(json.loads(row['report_metadata_json'])=={k:v for k,v in data.items() if k!='results'},'CSV metadata')
            return root/(prefix+'.html'),root/(prefix+'.csv')
        export(original,'actual');checks+=['actual_recovered_bytes','html_full_evidence_and_source','csv_full_evidence_and_source','csv_bom_and_rows']
        adversarial=json.loads(json.dumps(original));row=adversarial['results'][0]
        row.update(candidate_id='=1+2',path='\t=HYPERLINK("https://invalid.example")',archive_entry=' +1',error='<script>alert("test")</script>',warnings=['<img src=x onerror=alert(1)>','日本語, "引用"\r\n改行'],extent_evidence='estimated_contiguous',missing_ranges=[{'source_offset':512,'length':64,'reason':'failed'}])
        row['naming']['displayed_name']='<svg onload=alert(1)>日本語.txt';adversarial['source']='@SUM(1,2)'
        html,csv_path=export(adversarial,'hostile');check('Saved (estimated, unverified)' in html.read_text(encoding='utf-8'),'estimate was labelled verified')
        checks+=['html_injection_escaped','csv_formula_neutralized','unicode_quotes_newlines_preserved','estimated_saved_remains_unverified','missing_ranges_and_naming_preserved']
        for name,data in [('schema',{'schema_version':2,'results':[]}),('session',{'schema_version':1,'candidates':[]}),('bad-result',{'schema_version':1,'results':[{}]})]:
            bad=root/(name+'.json');bad.write_text(json.dumps(data));dest=root/(name+'.html')
            process=subprocess.run([str(binary),'export-report','--input',str(bad),'--output',str(dest),'--format','html','--json'],capture_output=True)
            check(process.returncode!=0 and not dest.exists(),'invalid report created output')
        checks.append('invalid_schema_session_and_row_refused')
        prior=file_hash(html)
        process=subprocess.run([str(binary),'export-report','--input',str(report),'--output',str(html),'--format','html','--json'],capture_output=True)
        check(process.returncode!=0 and file_hash(html)==prior,'existing report overwritten');checks.append('existing_output_preserved')
        huge=root/'large.json'
        with huge.open('wb') as f:f.truncate(16*1024*1024+1)
        dest=root/'large.html';process=subprocess.run([str(binary),'export-report','--input',str(huge),'--output',str(dest),'--format','html','--json'],capture_output=True)
        check(process.returncode!=0 and not dest.exists(),'oversize accepted');checks.append('bounded_input_refused')
        zipped=run(binary,'recover','--session',scan['session'],'--all','--archive',root/'files.zip','--json')
        with zipfile.ZipFile(root/'files.zip') as z:zip_report=json.loads(z.read('rescue-report.json'))
        export(zip_report,'archive');checks.append('zip_embedded_report_supported')
        check(file_hash(source)==before and file_hash(report)==report_before,'source/report changed');checks.append('source_and_json_unchanged')
    result={'status':'passed','version':subprocess.check_output([str(binary),'--version'],text=True).strip(),'binary_sha256':file_hash(binary),'checks':len(checks),'cases':checks,'files_byte_exact':exact,'source_unchanged':True,'scope':__doc__.strip()}
    if output:Path(output).write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result));return result
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path);a=p.parse_args();main(a.binary.resolve(),a.output)
