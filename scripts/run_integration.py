"""Run every integration suite, stopping on any failed subprocess on all shells."""
import argparse
from pathlib import Path
import subprocess
import sys
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);a=p.parse_args()
for name in ['e2e.py','e2e_streams.py','e2e_fat.py','e2e_carve.py','e2e_jpeg.py','e2e_documents.py','e2e_audit.py','e2e_features.py','e2e_restore.py','e2e_imaging.py','e2e_ntfs_extents.py','e2e_discovery.py','e2e_ntfs_remnants.py','e2e_raid.py','e2e_index.py','e2e_index_carve.py','e2e_index_fat.py']:
    subprocess.run([sys.executable,str(Path(__file__).parent/name),'--binary',str(a.binary)],check=True)
