"""Measure real X11 input-to-painted-result latency on an owned Rescue window.

The caller supplies reviewed screen coordinates and two independently captured
reference regions per action. The measured completion is a matching rendered
result, not merely input delivery or the return of an automation API. Templates
must differ. Screenshot polling and xdotool startup are included in latency.
Requires Pillow and xdotool; never selects host storage or starts a scan.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import threading
import time

from PIL import ImageGrab, Image


def process_tree(pid):
    result={pid}
    # Children can belong to any thread, not just the process leader.
    for task in (Path('/proc')/str(pid)/'task').glob('*'):
        try:children=map(int,(task/'children').read_text().split())
        except OSError:continue
        for child in children:result.update(process_tree(child))
    return result


def memory(pid):
    total=0
    for child in process_tree(pid):
        try:
            for line in (Path('/proc')/str(child)/'status').read_text().splitlines():
                if line.startswith('VmRSS:'):total+=int(line.split()[1])*1024
        except OSError:pass
    return total


def main(a):
    config=json.loads(a.config.read_text(encoding='utf-8'))
    if a.repetitions<3:raise ValueError('at least three repetitions required')
    env={**os.environ,'DISPLAY':a.display}
    def command(args):return subprocess.check_output([a.xdotool,*map(str,args)],env=env,text=True).strip()
    if int(command(['getwindowpid',a.window]))!=a.pid:raise ValueError('window does not belong to the supplied process')
    if 'Rescue' not in command(['getwindowname',a.window]):raise ValueError('not a Rescue window')
    command(['windowactivate','--sync',a.window])
    binary=a.binary.resolve()
    command_path=Path((Path('/proc')/str(a.pid)/'cmdline').read_bytes().split(b'\0')[0].decode()).resolve()
    if command_path!=binary:raise ValueError('supplied binary differs from running command')
    stop=threading.Event();peaks=[]
    def sample():
        while not stop.wait(.005):peaks.append(memory(a.pid))
    monitor=threading.Thread(target=sample);monitor.start()
    report={'status':'failed','platform':platform.platform(),'gui_binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),
            'pid':a.pid,'window':a.window,'display':a.display,'actions':{},'config':config,
            'scope':__doc__.strip()}
    try:
        for case in config['actions']:
            region=tuple(case['region']);states=case['states'];samples=[]
            if len(states)!=2:raise ValueError('each action requires two alternating states')
            templates=[Image.open(a.config.parent/s['template']).convert('RGB').tobytes() for s in states]
            if templates[0]==templates[1]:raise ValueError('rendered states must be distinguishable')
            if any(len(t)!=(region[2]-region[0])*(region[3]-region[1])*3 for t in templates):raise ValueError('template dimensions do not match reviewed region')
            def pixels():return ImageGrab.grab(bbox=region,xdisplay=a.display).convert('RGB').tobytes()
            # Establish the opposite state before timing the first operation.
            command(case.get('initial_command',states[1]['command']));deadline=time.monotonic()+10
            while pixels()!=templates[1]:
                if time.monotonic()>deadline:raise RuntimeError('initial rendered state mismatch: '+case['name'])
                time.sleep(.005)
            for iteration in range(a.repetitions*2):
                state=iteration%2;start=time.perf_counter();command(states[state]['command'])
                while pixels()!=templates[state]:
                    if time.perf_counter()-start>10:raise RuntimeError('rendered result timeout: '+case['name'])
                    time.sleep(.002)
                samples.append((time.perf_counter()-start)*1000)
            ordered=sorted(samples)
            report['actions'][case['name']]={'samples_ms':samples,'p95_ms':ordered[math.ceil(.95*len(ordered))-1],
                'median_ms':statistics.median(samples),'min_ms':min(samples),'max_ms':max(samples),
                'rendered_states_sha256':[hashlib.sha256(t).hexdigest() for t in templates]}
        report['status']='passed'
    except Exception as error:
        report['error']=str(error)
        raise
    finally:
        stop.set();monitor.join()
        report['aggregate_peak_rss_bytes']=max(peaks,default=memory(a.pid))
        report['rss_scope']='Rescue GUI and recursive descendants, all threads, sampled every 5 ms; no display-server/test-driver memory'
        a.output.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(json.dumps({k:v for k,v in report.items() if k in ['status','aggregate_peak_rss_bytes','error']}))


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--config',type=Path,required=True);p.add_argument('--pid',type=int,required=True)
    p.add_argument('--window',required=True);p.add_argument('--display',required=True);p.add_argument('--xdotool',default='xdotool')
    p.add_argument('--binary',type=Path,required=True);p.add_argument('--repetitions',type=int,default=20);p.add_argument('--output',type=Path,required=True);main(p.parse_args())
