"""Run every integration suite, stopping on any failed subprocess on all shells."""
import argparse
from pathlib import Path
import subprocess
import sys
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);a=p.parse_args()
for name in ['e2e.py','e2e_streams.py','e2e_fat.py','e2e_carve.py','e2e_audit.py','e2e_features.py']:
    subprocess.run([sys.executable,str(Path(__file__).parent/name),'--binary',str(a.binary)],check=True)
