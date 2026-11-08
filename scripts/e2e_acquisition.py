"""Real subprocess deadline, cancellation and identity-checked reconnect tests.

Faults are controlled suspension of this test's reader child and temporary renames
of disposable regular files, not claims about physical USB/SD failures.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
from queue import Queue, Empty
import signal
import subprocess
import tempfile
import threading
import time
from e2e import Client, check, run


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def children(parent):
    if os.name == 'nt':
        from ctypes import wintypes as w
        class Entry(ctypes.Structure):
            _fields_ = [('dwSize', w.DWORD), ('cntUsage', w.DWORD), ('th32ProcessID', w.DWORD),
                        ('th32DefaultHeapID', ctypes.c_size_t), ('th32ModuleID', w.DWORD),
                        ('cntThreads', w.DWORD), ('th32ParentProcessID', w.DWORD),
                        ('pcPriClassBase', w.LONG), ('dwFlags', w.DWORD), ('szExeFile', w.WCHAR * 260)]
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.CreateToolhelp32Snapshot.argtypes = [w.DWORD, w.DWORD]
        kernel.CreateToolhelp32Snapshot.restype = w.HANDLE
        kernel.Process32FirstW.argtypes = [w.HANDLE, ctypes.POINTER(Entry)]
        kernel.Process32NextW.argtypes = [w.HANDLE, ctypes.POINTER(Entry)]
        kernel.CloseHandle.argtypes = [w.HANDLE]
        snapshot = kernel.CreateToolhelp32Snapshot(2, 0)
        check(snapshot != ctypes.c_void_p(-1).value, 'process snapshot failed')
        found = []
        try:
            entry = Entry(); entry.dwSize = ctypes.sizeof(entry)
            valid = kernel.Process32FirstW(snapshot, ctypes.byref(entry))
            while valid:
                if entry.th32ParentProcessID == parent:
                    found.append(entry.th32ProcessID)
                valid = kernel.Process32NextW(snapshot, ctypes.byref(entry))
        finally:
            kernel.CloseHandle(snapshot)
        return found
    output = subprocess.check_output(['ps', '-axo', 'pid=,ppid='], text=True)
    return [int(line.split()[0]) for line in output.splitlines() if len(line.split()) == 2 and int(line.split()[1]) == parent]


def suspend_owned(parent):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        found = children(parent)
        if found:
            check(len(found) == 1, 'unexpected child processes')
            pid = found[0]
            if os.name == 'nt':
                from ctypes import wintypes as w
                kernel = ctypes.WinDLL('kernel32', use_last_error=True)
                kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]; kernel.OpenProcess.restype = w.HANDLE
                kernel.CloseHandle.argtypes = [w.HANDLE]
                handle = kernel.OpenProcess(0x0800, False, pid)
                check(bool(handle), 'cannot suspend owned child')
                try:
                    suspend = ctypes.WinDLL('ntdll').NtSuspendProcess
                    suspend.argtypes = [w.HANDLE]; suspend.restype = w.LONG
                    check(suspend(handle) == 0, 'NtSuspendProcess failed')
                finally:
                    kernel.CloseHandle(handle)
            else:
                os.kill(pid, signal.SIGSTOP)
            return pid
        time.sleep(.01)
    raise AssertionError('reader helper was not created')


def wait_gone(parent, pid):
    # Check the PID itself: an orphan is still a leak after its parent exits.
    until = time.monotonic() + 5
    while time.monotonic() < until:
        if os.name == 'nt':
            from ctypes import wintypes as w
            kernel = ctypes.WinDLL('kernel32', use_last_error=True)
            kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]; kernel.OpenProcess.restype = w.HANDLE
            kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
            kernel.CloseHandle.argtypes = [w.HANDLE]
            handle = kernel.OpenProcess(0x00100000, False, pid)
            if not handle:
                check(ctypes.get_last_error() == 87, 'cannot inspect reader lifetime')
                return
            try:
                if kernel.WaitForSingleObject(handle, 0) == 0: return
            finally:
                kernel.CloseHandle(handle)
        else:
            try: os.kill(pid, 0)
            except ProcessLookupError: return
        time.sleep(.02)
    raise AssertionError('timed-out helper was not reaped')


def start_paused(binary, source, destination, reconnect):
    command = [str(binary), 'image', '--source', str(source), '--destination', str(destination),
               '--block-size', '4096', '--read-timeout-ms', '3000', '--reconnect-wait-ms', str(reconnect), '--progress', '--json']
    child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding='utf-8')
    messages = Queue()
    def drain():
        for line in child.stderr:
            messages.put(json.loads(line))
    threading.Thread(target=drain, daemon=True).start()
    try:
        first = messages.get(timeout=10)
        check(first['map']['copied_bytes'] > 0, 'no durable initial progress')
        reader = suspend_owned(child.pid)
        return child, reader, messages
    except BaseException:
        child.kill(); child.wait(timeout=10)
        raise


def finish(child, expected, messages=None):
    code = child.wait(timeout=120 if expected == 0 else 15)
    output = child.stdout.read()
    check(code == expected, f'acquisition exit {code}, expected {expected}: {output}')
    if output.strip(): return json.loads(output)
    until = time.monotonic() + 5
    while time.monotonic() < until:
        event = messages.get(timeout=5)
        if "error" in event: return event
    raise AssertionError("no final error result")


def main(binary, output=None):
    cases = []
    with tempfile.TemporaryDirectory(prefix='rescue-adaptive-') as tmp:
        root = Path(tmp); source = root/'source.raw'
        with source.open('wb') as stream:
            for n in range(32): stream.write(bytes([n]) * 1024 * 1024)
        original = sha(source)
        child, helper, messages = start_paused(binary, source, root/'deadline', 0)
        started = time.monotonic()
        try:
            result = finish(child, 5)
            check(time.monotonic()-started < 8, 'deadline did not bound wait')
            check(result['state'] == 'partial' and result['stop_reason'], 'deadline not reported')
            check(sum(p['timeouts'] for p in result['passes']) == 1, 'timeout count')
            check(result['summary']['failed_bytes'] == 4096 and result['summary']['unattempted_bytes'] > 0, 'late read became copied data')
            wait_gone(child.pid, helper)
        finally:
            if child.poll() is None: child.kill(); child.wait(timeout=10)
        cases.extend(['actual reader wait deadline', 'late data excluded from durable map', 'timed-out helper reaped'])
        image = Path(result['image']); journal = Path(result['read_map'])
        before_map = journal.read_bytes()
        run(binary, 'image', '--source', source, '--destination', root/'deadline', '--block-size', '4096', '--resume', '--strategy', 'linear', '--json', expected=2)
        check(journal.read_bytes() == before_map, 'mismatched resume modified map')
        cases.append('strategy mismatch refused without mutation')
        resumed = run(binary, 'image', '--source', source, '--destination', root/'deadline', '--block-size', '4096', '--resume', '--json', timeout=120)
        check(sha(image) == original and resumed['summary']['strategy'] == 'adaptive', 'adaptive resume bytes')
        check(any(p['direction'] == 'reverse' for p in resumed['passes']), 'failed range not retried in reverse')
        cases.append('adaptive reverse retry and byte-exact resume')
        # Absence is transient; reopening is permitted only after the same inode/file ID returns.
        child, helper, messages = start_paused(binary, source, root/'reconnect', 5000)
        parked = root/'parked.raw'
        try:
            source.rename(parked)
            until = time.monotonic() + 8
            while time.monotonic() < until:
                event = messages.get(timeout=5)
                if event['phase'] == 'reconnect': break
            else: raise AssertionError('no reconnect progress')
            wait_gone(child.pid, helper)
            parked.rename(source)
            result = finish(child, 0)
            check(sha(Path(result['image'])) == original, 'reconnected image mismatch')
            check(sum(p['reconnects'] for p in result['passes']) == 1, 'reconnect not accounted')
        finally:
            if child.poll() is None: child.kill(); child.wait(timeout=10)
            if parked.exists(): parked.rename(source)
        cases.extend(['disconnect waiting progress', 'identical source reconnect', 'reconnected image byte exact'])
        child, helper, messages = start_paused(binary, source, root/'wrong-source', 5000)
        try:
            source.rename(parked)
            source.write_bytes(b'wrong source')
            result = finish(child, 4, messages)
            check(result.get('error', {}).get('code') == 'SOURCE_CHANGED', f'replacement identity accepted: {result}')
            wait_gone(child.pid, helper)
        finally:
            if child.poll() is None: child.kill(); child.wait(timeout=10)
            source.unlink(missing_ok=True); parked.rename(source)
        cases.append('replacement identity refused')
        # The old header has no strategy member. Independently remove it and rebuild its checksum.
        small = root/'small.raw'; small.write_bytes(bytes(range(256)) * 17 + b'tail')
        legacy = run(binary, 'image', '--source', small, '--destination', root/'legacy', '--strategy', 'linear', '--json')
        path = Path(legacy['read_map']); data = path.read_bytes(); count = int.from_bytes(data[8:12], 'little')
        header = json.loads(data[12:12+count]); del header['strategy']
        encoded = json.dumps(header, separators=(',', ':')).encode(); prefix = data[:8] + len(encoded).to_bytes(4, 'little') + encoded
        path.write_bytes(prefix + hashlib.sha256(prefix).digest() + data[12+count+32:])
        previous = sha(path)
        result = run(binary, 'image', '--source', small, '--destination', root/'legacy', '--resume', '--json')
        check(result['summary']['strategy'] == 'linear' and sha(path) == previous and sha(Path(result['image'])) == sha(small), 'legacy resume compatibility')
        cases.append('legacy header without strategy resumes unchanged')
        client = Client(binary, source, root/'mcp-work', root/'mcp-out')
        try:
            client.rpc('initialize', {'protocolVersion': '2025-11-25'})
            for field, value in [('strategy','bad'), ('read_timeout_ms',99), ('reconnect_wait_ms',300001)]:
                client.tool('start_image', {'source':str(source),'destination':str(root/'mcp-out/bad'),field:value,'request_id':field}, error=True)
            cases.append('MCP option bounds')
            task = client.tool('start_image', {'source':str(source),'destination':str(root/'mcp-out/cancel'),'block_size':4096,'read_timeout_ms':10000,'request_id':'cancel'})
            helper = suspend_owned(client.process.pid)
            started = time.monotonic()
            client.tool('cancel_job', {'job_id':task['job_id']})
            job = client.wait(task['job_id'])
            check(job['state'] == 'cancelled' and time.monotonic()-started < 5, 'cancel did not interrupt helper wait')
            wait_gone(client.process.pid, helper)
            check(job['result']['summary']['unattempted_bytes'] > 0, 'cancellation lost unread ranges')
            cases.append('MCP cancellation interrupts blocked read')
        finally:
            client.close()
        check(sha(source) == original, 'original changed'); cases.append('original source unchanged')
    report = {'status':'passed','schema_version':1,'platform':platform.platform(),'version':subprocess.check_output([str(binary),'--version'],text=True).strip(),
              'binary_sha256':sha(binary),'checks':len(cases),'cases':cases,'source_unchanged':True,'scope':'Actual CLI/MCP helper suspension and disposable regular-file renames. No physical media fault claim.'}
    if output: output.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
    print(json.dumps(report)); return report


if __name__ == '__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=Path,required=True);parser.add_argument('--output',type=Path)
    args=parser.parse_args();main(args.binary.resolve(),args.output)
