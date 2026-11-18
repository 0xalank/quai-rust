from pathlib import Path
import sys,json,gzip,os,tempfile,hashlib,subprocess,time
from lab import ROOT,sha,save
from verify_private_processes import Node,pack
import argparse,datetime,queue,shutil
parser=argparse.ArgumentParser();parser.add_argument('--small-oracle',type=Path,required=True);parser.add_argument('--large-oracle',type=Path);parser.add_argument('--development',action='store_true');parser.add_argument('--binary',type=Path);parser.add_argument('--out',type=Path);args=parser.parse_args();assert args.binary is None or args.development
r=ROOT;driver=sha(Path(__file__));helper=sha(r/'scripts/verify_private_processes.py');build=sha(r/'results/rust-build.json');metadata=json.loads((r/'results/rust-build.json').read_text())
if not args.development:
 for group in ['rust_sources','source_assets']:
  for name,h in metadata[group].items():assert sha(r/name)==h,'Rebuild first: '+name
small=args.small_oracle.resolve();small_meta=json.loads((small/'source.json').read_text());assert small_meta['passed']and small_meta['blocks']==96 and small_meta['validated_from_genesis']and not small_meta['checkpoint_imported']and small_meta['profile_sha256']==sha(r/'configs/private-execution-v2.json')and small_meta['compressed_stream_sha256']==sha(small/'blocks.jsonl.gz')
for name,h in small_meta['oracle_sources'].items():assert sha(r/name)==h,name
out=args.out.resolve()if args.out else r/'results'/('forward-processes-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));assert out.is_relative_to(r/'results')and not out.exists();out.mkdir();shutil.copy2(Path(__file__),out/'qualification-driver.py');shutil.copy2(r/'scripts/verify_private_processes.py',out/'process-helper.py')
if args.binary is None:
 with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-network','--features','native-transport','--example','private-node'],cwd=r,stdout=log,stderr=subprocess.STDOUT,check=True)
 original=Path(os.environ.get('CARGO_TARGET_DIR',r/'target'))/'release/examples/private-node'
else:original=args.binary.resolve()
binary=out/'quai-private-node';shutil.copy2(original,binary);binary_hash=sha(binary);v=json.loads((r/'fixtures/protocol/canonical.json').read_text());rows=[json.loads(x)for x in gzip.open(small/'blocks.jsonl.gz','rt')];now=rows[0]['now'];blocks=rows[1:-1];nodes=[];checks=[];large_result=None
def check(ok,why):assert ok,why;checks.append(why)
def roots(node,row):
 s=node.status();check(s['height']==row.get('height',16),'native selected height');
 for k in ['evm_root','etx_root','utxo_root']:check(s[k]==row[k],'native '+k)
 return s
with tempfile.TemporaryDirectory(prefix='quai-forward-native-')as directory:
 d=Path(directory);deny=d/'deny';deny.mkdir();marker=d/'forbidden'
 for name in ['go','gofmt','protoc']:
  p=deny/name;p.write_text('#!/bin/sh\n: > '+str(marker)+'\nexit 97\n');p.chmod(0o755)
 env={**os.environ,'PATH':str(deny)+os.pathsep+os.environ['PATH'],'GO_QUAI_DIR':str(d/'absent-go')}
 def start(name,store=None,clock=None):return Node(binary,d/(store or name),now if clock is None else clock,env,out,name,nodes,profile='private-execution-v2')
 def follow(n,source,head,height=96,**kw):return n.call('follow',peer_id=source.ready['peer_id'],address=source.ready['address'],head=head,height=height,**kw)
 try:
  source=start('source');receiver=start('receiver');cp=d/'pilot.json';cp.write_text(json.dumps(pack(blocks)));source.call('import',path=str(cp));source.call('enroll',peer_id=receiver.ready['peer_id']);head=rows[-1]['head']
  result=follow(receiver,source,head,window_blocks=7);check(result['imported_blocks']==96 and result['windows']==14 and result['target_admitted'],'96 native blocks across fourteen bounded windows');check(result['head']==head,'forward head matches offline Go');roots(receiver,blocks[-1]);identity=receiver.ready['peer_id'];receiver.stop();receiver=start('receiver-reopen',store='receiver');check(receiver.ready['peer_id']==identity,'v2 identity survives forward reopen');roots(receiver,blocks[-1]);
  known=receiver.call('follow',peer_id=source.ready['peer_id'],address='/ip4/127.0.0.1/tcp/1',head=head,height=96,window_blocks=7);check(known['requests']==0 and known['windows']==0,'known head needs no forward connection')
  bad=receiver.call('follow',ok=False,peer_id=source.ready['peer_id'],address=source.ready['address'],head=head,height=95);check('height'in str(bad),'known forward target cannot substitute a height');roots(receiver,blocks[-1])
  receiver.stop();receiver.cleanup();nodes.remove(receiver)
  # Cancellation interrupts only between durable native block publications.
  cancel=start('cancel');source.call('enroll',peer_id=cancel.ready['peer_id']);job=cancel.send('follow',peer_id=source.ready['peer_id'],address=source.ready['address'],head=head,height=96,window_blocks=7)
  deadline=time.monotonic()+30
  while cancel.call('status')['admitted_blocks']<5:assert time.monotonic()<deadline;time.sleep(.002)
  cancel.call('cancel');reply=cancel.wait(job);check(not reply['ok']and 'cancel'in str(reply['error']),'forward cancellation reports incomplete acquisition');s=cancel.status();check(0<s['height']<96,'forward cancellation keeps a complete native prefix');h=s['height'];cancel.stop();cancel=start('cancel-reopen',store='cancel');check(cancel.status()['height']==h,'forward cancelled prefix survives writer reopen');resumed=follow(cancel,source,head,window_blocks=7);check(resumed['imported_blocks']==96-h,'forward resume starts at native common ancestor');roots(cancel,blocks[-1])
  crash=start('crash');source.call('enroll',peer_id=crash.ready['peer_id']);crash.send('follow',peer_id=source.ready['peer_id'],address=source.ready['address'],head=head,height=96,window_blocks=7)
  deadline=time.monotonic()+30
  while True:
   progress=crash.call('status')['admitted_blocks']
   if progress>=12:break
   assert time.monotonic()<deadline;time.sleep(.002)
  crash.kill();crash.cleanup();nodes.remove(crash);crash=start('crash-reopen',store='crash');recovered=crash.status();check(progress-1<=recovered['height']<96,'SIGKILL recovers the complete published forward prefix');roots(crash,blocks[recovered['height']-1]);again=follow(crash,source,head,window_blocks=7);check(again['imported_blocks']==96-recovered['height'],'SIGKILL forward prefix resumes without rebuilding committed ancestry');roots(crash,blocks[-1])
  # Native canonical fork choice remains independent of the remote selected head.
  a=start('fork-a');b=start('fork-b');low=start('lower-score-receiver')
  for n,index in [(a,v['first_branch']),(b,v['winning_branch'])]:
   p=d/('fork-'+str(index)+'.json');p.write_text(json.dumps(pack(v['branches'][index]['vectors'])));n.call('import',path=str(p));n.call('enroll',peer_id=low.ready['peer_id'])
  high=v['canonical_events'][-1]['head'];low_head=v['canonical_events'][15]['head'];follow(low,b,high,height=16,window_blocks=3);one=follow(low,a,low_head,height=16,window_blocks=3);check(one['imported_blocks']==8 and one['windows']==3 and one['selected_changes']==0 and one['head']==high and one['target_admitted'],'remote lower-score fork is admitted without overriding native selection');
  unknown=start('unknown-target');a.call('enroll',peer_id=unknown.ready['peer_id']);err=unknown.call('follow',ok=False,peer_id=a.ready['peer_id'],address=a.ready['address'],head=high,height=16);check('target'in str(err),'unknown advertised head must match remote canonical height index');check(unknown.status()['height']==0 and unknown.status()['admitted_blocks']==1,'failed forward discovery cannot publish a prefix')
  err=low.call('follow',ok=False,peer_id=a.ready['peer_id'],address=a.ready['address'],head=high,height=17);check('height'in str(err)or'target'in str(err)or'hash'in str(err),'incorrect forward height/head is refused');check(low.status()['head']==high,'invalid forward discovery preserves selected head');check(not marker.exists(),'native follow runtime never invokes Go or protoc')
  if args.large_oracle:
   large=args.large_oracle.resolve();lm=json.loads((large/'source.json').read_text());check(lm['passed']and lm['blocks']==8192 and lm['validated_from_genesis']and not lm['checkpoint_imported'],'8192-block completed native Go genesis oracle');assert lm['compressed_stream_sha256']==sha(large/'blocks.jsonl.gz')and lm['profile_sha256']==sha(r/'configs/private-execution-v2.json')
   for name,h in lm['oracle_sources'].items():assert sha(r/name)==h,name
   big_source=start('large-source',clock=lm['now']);big_receiver=start('large-receiver',clock=lm['now']);batch=[];last=None;batch_file=d/'large-pack.json';count=0
   with gzip.open(large/'blocks.jsonl.gz','rt')as f:
    for raw in f:
     row=json.loads(raw)
     if row['kind']!='block':continue
     batch.append(row);count+=1;last=row
     if len(batch)==128:
      batch_file.write_text(json.dumps(pack(batch)));q=big_source.call('import',path=str(batch_file));check(q['imported_blocks']==len(batch),'bounded native source import batch');batch.clear()
      if count%1024==0:roots(big_source,last);print(json.dumps({'large_source_height':count}),flush=True)
   assert not batch and count==8192 and last is not None;check(big_source.status()['head']==lm['head'],'large native source matches Go head');big_source.call('enroll',peer_id=big_receiver.ready['peer_id']);job=big_receiver.send('follow',peer_id=big_source.ready['peer_id'],address=big_source.ready['address'],head=lm['head'],height=8192,window_blocks=128);end=time.monotonic()+3600;last_progress=0;last_change=time.monotonic()
   while True:
    try:reply=big_receiver.wait(job,timeout=5);break
    except queue.Empty:
     assert big_receiver.p.poll()is None;status=big_receiver.call('status');n=status['admitted_blocks']-1
     if n>last_progress:last_progress=n;last_change=time.monotonic();print(json.dumps({'large_receiver_native_admissions':n}),flush=True)
     assert time.monotonic()<end and time.monotonic()-last_change<120,'forward native sync stalled'
   check(reply['ok'],'large forward worker finishes');large_result=reply['result'];check(large_result['imported_blocks']==8192 and large_result['windows']==64 and large_result['requests']==12802,'forward windows cross the previous 4095-block acquisition cap');check(large_result['head']==lm['head']and large_result['target_admitted'],'large receiver independently selects exact Go head');check(large_result['peak_window_blocks']<=128 and large_result['peak_window_response_bytes']<=32<<20,'large forward acquisition stays within each window budget');roots(big_receiver,last);big_receiver.stop();big_receiver=start('large-receiver-reopen',store='large-receiver',clock=lm['now']);roots(big_receiver,last);check(big_receiver.status()['admitted_blocks']==8193,'large native receipt catalog survives process reopen');shutil.copy2(large/'source.json',out/'large-oracle-source.json')
 finally:
  for n in nodes:n.cleanup()
assert driver==sha(Path(__file__))and helper==sha(r/'scripts/verify_private_processes.py')and binary_hash==sha(binary)
save(out/'checks.json',checks);shutil.copy2(small/'source.json',out/'small-oracle-source.json')
report={'passed':True,'build_sha256':build if not args.development else None,'qualified_source_build':not args.development,'reference_revision':metadata['reference_revision'],'assertions':len(checks),'small_genesis_blocks':96,'small_forward_result':result,'large_forward_result':large_result,'genesis_to_activation_qualified':False,'profile':'private-execution-v2','profile_sha256':sha(r/'configs/private-execution-v2.json'),'driver_sha256':driver,'process_helper_sha256':helper,'node_binary_sha256':binary_hash,'node_artifact':str(binary.relative_to(r)),'separate_os_processes':True,'window_block_bound':128,'window_response_byte_bound':32<<20,'cancellation_and_resume_checked':True,'sigkill_prefix_recovery_checked':True,'lower_score_remote_cannot_override_native_head':True,'known_head_needs_no_connection':True,'go_commands_attempted':0,'private_key_contents_in_artifacts':False,'full_consensus':False,'canonical_public_sync':False,'public_network':False,'scope':'Independent Rust processes import native Go genesis data and exchange raw numbered blocks/context witnesses in bounded windows. Native validation/fork choice decides all publications. This is an experimental private pre-activation corpus; transaction activation, other ETX kinds, later forks, nonlocal/expansion and automatic public following remain unqualified. Peak window counters count response bytes and raw bundles, not heap or RSS.','evidence_files':{p.name:sha(p)for p in out.iterdir()if p.is_file()}};save(out/'summary.json',report)
if not args.development:save(r/'results/latest-forward-processes.json',{'passed':True,'directory':str(out.relative_to(r))})
print(json.dumps({'passed':True,'assertions':len(checks),'small_requests':result['requests'],'large_blocks':large_result['imported_blocks']if large_result else None,'full_consensus':False},indent=2))
