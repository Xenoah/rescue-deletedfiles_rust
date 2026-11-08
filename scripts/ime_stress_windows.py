"""Keep a normal desktop permission boundary across fresh native GUI processes.

The hosted service desktop initially grants access through Administrators only.
Keep the existing fixture's own-user grant for the full batch so shared Windows
input services do not lose desktop access between app exits. Restore both exact
original ACLs once at the end, including after failure. Each GUI still receives
its own restricted medium token, process job and independent recovery fixture.
"""
import argparse,json,subprocess,sys
from pathlib import Path
from windows_test_user import desktop_test_user
from windows_desktop import temporary_desktop_access

p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);args=p.parse_args()
record={'status':'failed','scope':__doc__.strip(),'sessions':[]}
result=1
try:
    with desktop_test_user() as identity, temporary_desktop_access(identity['token']) as desktop:
        record['desktop_permissions']=desktop
        sessions=[('native',[]),('limited',['--desktop-token']),('keyboard',['--desktop-token','--keyboard'])]
        sessions += [(f'ime-{i}',['--desktop-token','--ime','--ime-cycles','8']) for i in range(1,5)]
        for name,options in sessions:
            command=[sys.executable,'scripts/ci_command.py',sys.executable,'scripts/e2e_gui_windows.py','--binary',str(args.binary),'--output',f'trace-{name}.json',*options]
            completed=subprocess.run(command)
            record['sessions'].append({'name':name,'returncode':completed.returncode})
            if completed.returncode:
                if name.startswith('ime-'):
                    # Observe the standard control while the same desktop
                    # permission boundary remains in force; never retry Rescue.
                    baseline=subprocess.run([sys.executable,'scripts/ci_command.py',sys.executable,'scripts/ime_wpf_baseline.py','--close-only'])
                    record['wpf_baseline_returncode']=baseline.returncode
                raise RuntimeError('Fresh native GUI session failed: '+name)
        record['status']='passed';result=0
except Exception as error:
    record['status']='failed';record['error']=str(error);result=1
finally:
    Path('trace-desktop-lifetime.json').write_text(json.dumps(record,indent=2)+'\n',encoding='utf-8')
raise SystemExit(result)
