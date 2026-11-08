"""Run an owned standard WPF control under the same restricted desktop token."""
import json,os,subprocess,tempfile
from pathlib import Path
from windows_test_user import desktop_test_user
from windows_restricted import run_limited

root=Path(tempfile.mkdtemp(prefix='rescue-owned-uia-wpf-',dir='.')).resolve()
config=root/'config.json';result=root/'uia.json'
config.write_text(json.dumps({'root':str(root),'result':str(result),'query':'原文'},ensure_ascii=False),encoding='utf-8')
powershell=Path(os.environ['SystemRoot'])/'System32/WindowsPowerShell/v1.0/powershell.exe'
with desktop_test_user() as identity:
    subprocess.run(['icacls',str(root),'/grant','*'+identity['sid']+':(OI)(CI)M'],check=True,capture_output=True)
    command=[str(powershell),'-Mta','-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',str(Path(__file__).with_suffix('.ps1').resolve()),'-Config',str(config)]
    completed=run_limited(command,token=identity['token'],timeout=180,kill_tree=True,restricted=identity.get('restricted',False))
(root/'driver-stderr.txt').write_text(completed.stderr,encoding='utf-8')
(root/'driver-stdout.txt').write_text(completed.stdout,encoding='utf-8')
print(result.read_text(encoding='utf-8-sig') if result.exists() else completed.stderr)
raise SystemExit(completed.returncode)
