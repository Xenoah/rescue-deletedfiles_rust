"""Run every integration suite, stopping on any failed subprocess on all shells."""
import argparse
from pathlib import Path
import subprocess
import sys
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--index-reports',type=Path);a=p.parse_args()
if a.index_reports:a.index_reports.mkdir(parents=True,exist_ok=True)
for name in ['e2e.py','e2e_streams.py','e2e_fat.py','e2e_carve.py','e2e_jpeg.py','e2e_documents.py','e2e_audit.py','e2e_features.py','e2e_restore.py','e2e_imaging.py','e2e_ntfs_extents.py','e2e_discovery.py','e2e_ntfs_remnants.py','e2e_raid.py','e2e_index.py','e2e_index_carve.py','e2e_index_fat.py','e2e_index_remnants.py','e2e_index_late.py','e2e_index_discovery.py','e2e_preview.py','e2e_allocation_scope.py','e2e_job_contract.py','e2e_naming.py','e2e_source_contract.py','e2e_export_raw.py','e2e_damaged_fat.py','e2e_classic_fat.py','e2e_estimated_fat.py','e2e_reports.py','e2e_release_fixes.py','e2e_acquisition.py','e2e_cp932.py','e2e_exfat_damage.py']:
    arguments=[sys.executable,str(Path(__file__).parent/name),'--binary',str(a.binary)]
    if a.index_reports and (name.startswith('e2e_index') or name in ['e2e_preview.py','e2e_allocation_scope.py','e2e_job_contract.py','e2e_naming.py','e2e_source_contract.py','e2e_export_raw.py','e2e_damaged_fat.py','e2e_classic_fat.py','e2e_estimated_fat.py','e2e_reports.py','e2e_release_fixes.py','e2e_acquisition.py','e2e_cp932.py','e2e_exfat_damage.py']):
        arguments.extend(['--output',str(a.index_reports/(Path(name).stem+'.json'))])
    print('Running '+name,flush=True)
    subprocess.run(arguments,check=True)
