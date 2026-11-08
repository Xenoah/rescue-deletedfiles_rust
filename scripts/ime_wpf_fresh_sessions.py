"""Observe two standard WPF processes on a host that has never run Rescue.

Diagnostic only: a baseline failure is preserved, and the second independent
control still runs to distinguish startup from process-restart behavior.
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

from windows_desktop import temporary_desktop_access
from windows_test_user import desktop_test_user

parser = argparse.ArgumentParser()
parser.add_argument('--close-only', action='store_true')
args = parser.parse_args()
record = {'scope': __doc__.strip(), 'status': 'failed', 'sessions': [], 'restore_layout': not args.close_only}
result = 1
try:
    with desktop_test_user() as identity, temporary_desktop_access(identity['token']) as desktop:
        record['desktop_permissions'] = desktop
        for session in range(1, 3):
            before = set(Path('.').glob('rescue-owned-uia-wpf-*'))
            completed = subprocess.run([
                sys.executable, 'scripts/ci_command.py', sys.executable,
                'scripts/ime_wpf_baseline.py', *(['--close-only'] if args.close_only else []),
            ])
            created = set(Path('.').glob('rescue-owned-uia-wpf-*')) - before
            record['sessions'].append({
                'session': session, 'returncode': completed.returncode,
                'evidence_directories': sorted(str(path) for path in created),
            })
        if all(item['returncode'] == 0 for item in record['sessions']):
            record['status'] = 'passed'
            result = 0
except Exception as error:
    record['status'] = 'failed'
    record['error'] = str(error)
    result = 1
finally:
    Path('wpf-fresh-sessions.json').write_text(json.dumps(record, indent=2) + '\n', encoding='utf-8')
raise SystemExit(result)
