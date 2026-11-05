"""Keep failed native build diagnostics visible in the public Actions summary."""
import os
from pathlib import Path
import subprocess
import sys
sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.stderr.reconfigure(encoding="utf-8", errors="replace")
result=subprocess.run(sys.argv[1:],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,encoding='utf-8',errors='replace')
print(result.stdout,end='',flush=True)
if result.returncode:
    tail='\n'.join(result.stdout.splitlines()[-100:])[-14000:]
    escaped=tail.replace('%','%25').replace('\r','%0D').replace('\n','%0A')
    print('::error title=Native command failed::'+escaped)
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with Path(os.environ['GITHUB_STEP_SUMMARY']).open('a',encoding='utf-8') as f:f.write('\n### Failed native check\n\n```text\n'+tail+'\n```\n')
raise SystemExit(result.returncode)
