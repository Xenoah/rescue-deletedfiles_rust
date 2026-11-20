"""Measure whole-file SHA-256 recovery on the 35-format corpus after metadata or bytes are erased."""
import argparse
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

from e2e import check
from e2e_formats import corpus_load, ntfs_image
from make_fixture import MFT_LCN, SECTOR, RECORD


def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f,'sha256').hexdigest()


def measure(binary,corpus,root,regular_strides=False):
    files=list(corpus_load(corpus))
    report={'cli_version':subprocess.check_output([binary,'--version'],text=True).strip(),
            'cli_sha256':sha(binary),'formats':len({f['format'] for f in files}),'originals':len(files),
            'logical_sector_bytes':512,'disk_images':{},'cases':{}}
    layouts = ['fragmented','overwritten'] if regular_strides else ['contiguous','fragmented','overwritten']
    for layout in layouts:
        image=root/(layout+'.raw')
        truth,meta=ntfs_image(image,files,'fragmented' if regular_strides or layout=='fragmented' else 'contiguous')
        # Keep file data for the first two scenarios. Erase boot record and every MFT record.
        with image.open('r+b') as f:
            f.write(bytes(512));f.seek(MFT_LCN*SECTOR);f.write(bytes(128*RECORD))
            if layout=='overwritten':
                for spans in meta['extents'].values():
                    for offset,length in spans:
                        f.seek(offset);f.write(bytes(length))
        before=sha(image);report['disk_images'][layout]=before
        for anchored in ([False,True] if regular_strides or layout=='fragmented' else [False]):
            scenario=layout+('-known-start' if anchored else '-unknown-start')
            if regular_strides:
                scenario+='-detected-step' if anchored else '-fixed-step'
            rows={};folder=root/scenario;folder.mkdir()
            for key,item in truth.items():
                output=folder/item['path'].name
                # Search the entire disk from offset zero. The oracle's extent data is
                # only consulted in the explicitly anchored trial and for constructing the overwrite.
                work=512<<20 if anchored else 1<<30
                args=[binary,'recover-by-hash','--source',image,'--output',output,'--sha256',item['sha256'],
                      '--file-bytes',str(item['bytes']),'--block-size','512','--max-work',str(work),'--json']
                if anchored:args+=['--first-offset',str(meta['extents'][key][0][0])]
                if regular_strides:
                    args += ['--max-stride-blocks','8'] if anchored else ['--stride-blocks','2']
                r=subprocess.run(args,capture_output=True,text=True,encoding='utf-8',timeout=180)
                row=dict(original_bytes=item['bytes'],original_sha256=item['sha256'])
                if r.returncode==0:
                    result=json.loads(r.stdout)
                    check(output.read_bytes()==item['data'] and sha(output)==result['file']['sha256']==item['sha256'],output)
                    row.update(result='byte_exact',spans=len(result['extents']),candidates=result['candidates_tried'],
                               search_bytes=result['search_bytes'])
                else:
                    check(r.returncode==4 and not output.exists(),(scenario,output,r.stderr))
                    if 'byte budget exhausted' in r.stderr or 'candidate limit' in r.stderr:
                        row.update(result='search_limit')
                    else:
                        check('no whole-file SHA-256 match' in r.stderr,r.stderr)
                        row.update(result='no_match')
                if regular_strides and r.returncode==0:
                    row['matched_stride']=result['matched_stride']
                rows[output.name]=row
            totals={k:sum(row['result']==k for row in rows.values()) for k in ['byte_exact','no_match','search_limit']}
            report['cases'][scenario]=dict(search_budget_per_original=work,totals=totals,files=rows)
            check(sha(image)==before,scenario+' changed the input')
            if layout=='contiguous' or (regular_strides and layout=='fragmented'):check(totals['byte_exact']==len(files),totals)
            if layout=='overwritten':check(totals['no_match']==len(files),totals)
            print(scenario,totals,flush=True)
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--regular-strides',action='store_true',
                        help='test a fixed sector spacing with unknown start and a bounded spacing search with known start')
    args=parser.parse_args()
    with tempfile.TemporaryDirectory() as temp:
        report=measure(args.binary.resolve(),Path(__file__).resolve().parents[1]/'tests/corpus',Path(temp),args.regular_strides)
    args.output.write_text(json.dumps(report,indent=2)+'\n')
