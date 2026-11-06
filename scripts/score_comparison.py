"""Score actual recovered files against an immutable, independently held oracle.

No product-specific success labels are trusted. Missing files remain in the
denominator. Duplicate outputs never increase recovery counts. Metadata files
and unknown outputs are reported separately; absence of unknown outputs alone
is not a claim of zero false-positive scan candidates.
"""
import argparse
import hashlib
import json
from pathlib import Path


def digest(path):
    h=hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda:source.read(1024*1024),b''):h.update(block)
    return h.hexdigest()


def read_manifest(path):
    manifest=json.loads(path.read_text(encoding='utf-8'))
    for case in manifest['cases']:
        case['source']=str((path.parent/Path(case['source'])).resolve())
        for item in case['files']:
            item['original']=str((path.parent/Path(item['original'])).resolve())
            original=Path(item['original'])
            if original.stat().st_size!=item['size'] or digest(original)!=item['sha256']:
                raise ValueError('oracle file changed: '+item['name'])
    return manifest


def score(case, directory):
    expected=case['files']
    outputs=[{'path':p.relative_to(directory).as_posix(),'size':p.stat().st_size,'sha256':digest(p)}
             for p in sorted(directory.rglob('*')) if p.is_file()]
    rows=[];used=set()
    for item in expected:
        exact=[o for o in outputs if o['size']==item['size'] and o['sha256']==item['sha256']]
        named=[o for o in outputs if Path(o['path']).name==item['name']]
        candidate=next((o for o in exact if o['path']==item['relative_path']),None)
        candidate=candidate or next(iter(exact),None) or next(iter(named),None)
        matched=0
        if candidate:
            used.add(candidate['path'])
            original=Path(item['original']).read_bytes()
            actual=(directory/candidate['path']).read_bytes()
            matched=sum(a==b for a,b in zip(original,actual))
        rows.append({'id':item['id'],'name':item['name'],'expected_bytes':item['size'],
                     'byte_exact':bool(exact),'matching_bytes':matched,
                     'name_exact':any(Path(o['path']).name==item['name'] for o in exact),
                     'relative_path_exact':any(o['path']==item['relative_path'] for o in exact),
                     'output':candidate,'duplicate_exact_outputs':max(0,len(exact)-1)})
        used.update(o['path'] for o in exact)
    extra=[o for o in outputs if o['path'] not in used]
    # Known filesystem metadata can be selected by commercial "select all";
    # Rescue's report is a sidecar, not a recovered candidate.
    metadata=[o for o in extra if any(p.startswith('$') for p in Path(o['path']).parts)
              or Path(o['path']).name.startswith('rescue-report')]
    unknown=[o for o in extra if o not in metadata]
    return {'files_expected':len(expected),'bytes_expected':sum(e['size'] for e in expected),
            'files_byte_exact':sum(r['byte_exact'] for r in rows),
            'matching_bytes':sum(r['matching_bytes'] for r in rows),
            'names_exact':sum(r['name_exact'] for r in rows),
            'relative_paths_exact':sum(r['relative_path_exact'] for r in rows),
            'duplicate_exact_outputs':sum(r['duplicate_exact_outputs'] for r in rows),
            'known_metadata_outputs':metadata,'unclassified_outputs':unknown,'files':rows}


def main(a):
    manifest=read_manifest(a.manifest);reports=[]
    for case in manifest['cases']:
        if a.case and case['case'] not in a.case:continue
        actual=digest(Path(case['source']))
        if actual!=case['source_sha256']:raise ValueError('comparison source changed: '+case['case'])
        result={'case':case['case'],'source_sha256':actual,'source_unchanged':True,'scope':case.get('scope'), 'products':{}}
        for product in a.product:
            directory=a.outputs/product/case['case']
            if not directory.is_dir():raise ValueError('missing actual output directory: '+str(directory))
            result['products'][product]=score(case,directory)
        reports.append(result)
    if not reports:raise ValueError('no comparison cases selected')
    a.output.write_text(json.dumps({'schema_version':1,'status':'scored','scope':__doc__.strip(),'cases':reports},ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    for case in reports:
        print(case['case'],{p:f"{v['files_byte_exact']}/{v['files_expected']}" for p,v in case['products'].items()})


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--manifest',type=Path,required=True);p.add_argument('--outputs',type=Path,required=True)
    p.add_argument('--product',action='append',required=True);p.add_argument('--case',action='append');p.add_argument('--output',type=Path,required=True)
    main(p.parse_args())
