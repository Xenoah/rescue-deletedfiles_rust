"""Measure a real GUI scan and three pointer cancellations on an owned sparse image.

First configure the running GUI with the fixture source, separate destination
and index directory. Review screen coordinates in --config (start_scan, cancel,
resume, select_first, version); this script clicks only that reviewed window.
Requires the make_large_fixture.py oracle and an empty destination for a new
index. Uses read-only SQLite checkpoint inspection and samples product-tree RSS.
"""
import argparse,os,time,json,hashlib,sqlite3,subprocess,threading
from pathlib import Path
from PIL import ImageGrab
from benchmark_gui import memory
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--output-dir',type=Path,required=True);p.add_argument('--index-dir',type=Path,required=True)
p.add_argument('--binary',type=Path,required=True);p.add_argument('--pid',type=int,required=True)
p.add_argument('--source',type=Path,required=True);p.add_argument('--display',required=True);p.add_argument('--window',required=True)
p.add_argument('--config',type=Path,required=True);p.add_argument('--xdotool',default='xdotool')
a=p.parse_args();root=a.output_dir.resolve();root.mkdir(parents=True,exist_ok=True);folder=a.index_dir.resolve();old=set(folder.glob('*/session.sqlite'));binary=a.binary.resolve();pid=a.pid
config=json.loads(a.config.read_text());oracle=json.loads(a.source.with_suffix('.truth.json').read_text());initial=a.source.stat()
if not a.source.is_file() or initial.st_size!=oracle['logical_bytes']:raise ValueError('source does not match owned fixture oracle')
os.environ['DISPLAY']=a.display;xd=a.xdotool
if int(subprocess.check_output([xd,'getwindowpid',a.window],text=True))!=pid:raise ValueError('window PID mismatch')
command=Path((Path('/proc')/str(pid)/'cmdline').read_bytes().split(b'\0')[0].decode()).resolve()
if command!=binary:raise ValueError('running GUI binary mismatch')
subprocess.run([xd,'windowactivate','--sync',a.window],check=True)
def click(x,y):subprocess.run([xd,'mousemove',str(x),str(y),'click','1'],check=True)
def shot(name):ImageGrab.grab(xdisplay=a.display).save(root/name)
stop=threading.Event();peaks=[]
def monitor():
 while not stop.wait(.005):peaks.append(memory(pid))
t=threading.Thread(target=monitor);t.start();report={'status':'failed','version':config['version'],'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'pid':pid,'cancel_runs':[]};start=time.monotonic()
try:
 click(*config['start_scan']);deadline=time.monotonic()+20
 while not (set(folder.glob('*/session.sqlite'))-old):
  if time.monotonic()>deadline:raise RuntimeError('no new GUI index')
  time.sleep(.05)
 path=next(iter(set(folder.glob('*/session.sqlite'))-old));report['index']=str(path)
 def state():
  with sqlite3.connect('file:'+str(path)+'?mode=ro',uri=True,timeout=1) as c:return c.execute('select state,resumable,watermark from checkpoint').fetchone()
 def await_rows(minimum):
  end=time.monotonic()+120
  while time.monotonic()<end:
   try:s=state()
   except sqlite3.OperationalError:time.sleep(.05);continue
   if s[2]>=minimum:return s
   time.sleep(.05)
  raise RuntimeError('scan failed to advance')
 baseline=0
 for i in range(3):
  running=await_rows(baseline+25000);shot(f'v1-gui-running-{i}.png');begin=time.monotonic();click(*config['cancel']);end=time.monotonic()+10
  while True:
   paused=state()
   if paused[0]=='cancelled':break
   if time.monotonic()>end:raise RuntimeError('cancel did not persist')
   time.sleep(.005)
  elapsed=time.monotonic()-begin;time.sleep(.5);unchanged=state()==paused
  if not unchanged:raise RuntimeError('new records committed after cancellation')
  if i==0:click(*config['select_first'])
  shot(f'v1-gui-paused-{i}.png');report['cancel_runs'].append({'seconds':elapsed,'watermark':paused[2],'checkpoint_unchanged_after_500_ms':unchanged});baseline=paused[2]
  click(*config['resume'])
 last=-1;deadline=time.monotonic()+1200
 while time.monotonic()<deadline:
  s=state()
  if s[2]//1000000!=last:
   last=s[2]//1000000;print(json.dumps({'state':s,'seconds':round(time.monotonic()-start,1),'rss_peak':max(peaks)}),flush=True)
  if s[0]!='running':
   if s[0]!='completed' or s[1] or s[2]!=oracle['candidates']:raise RuntimeError('GUI scan stopped early: '+str(s))
   report['final_checkpoint']=s;break
  time.sleep(.5)
 else:raise RuntimeError('full scan timeout')
 time.sleep(1);shot('v1-gui-10m-complete.png');report['elapsed_seconds_including_three_cancellations']=time.monotonic()-start;report['index_bytes']=path.stat().st_size;final=a.source.stat();report['source_size_and_mtime_unchanged']=(initial.st_size,initial.st_mtime_ns)==(final.st_size,final.st_mtime_ns);report['status']='passed'
except Exception as e:report['error']=str(e);shot('v1-gui-scan-failure.png');raise
finally:
 stop.set();t.join();report['aggregate_peak_rss_bytes']=max(peaks,default=memory(pid));report['scope']='Actual X11 GUI scan of 1 TiB sparse synthetic NTFS with 10 million resident files. Product process-tree RSS every 5 ms. Three pointer cancellations and resumes; durable checkpoint timing includes xdotool overhead. No physical 1 TiB throughput or failing-device timing claim.';(root/'v1-gui-full-scan.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report),flush=True)
