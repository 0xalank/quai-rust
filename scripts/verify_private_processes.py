#!/usr/bin/env python3
"""Independent private-profile node processes; no shared store/cache/Go runtime."""
import datetime,json,os,queue,shlex,shutil,subprocess,tempfile,threading,time
from pathlib import Path
from lab import ROOT,sha,save
DRIVER_DIGEST=sha(Path(__file__))

class Node:
 def __init__(self, binary, directory, now, env, out, name, registry, profile="private-hierarchy-v1"):
  self.directory=directory;self.queue=queue.Queue();self.saved={};self.serial=0;self.name=name
  self.err=(out/(name+'-stderr.log')).open('w');self.transcript=(out/(name+'-control.jsonl')).open('w')
  self.p=subprocess.Popen([str(binary),'--profile',profile,'--datadir',str(directory),'--now',str(now)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.err,text=True,env=env,bufsize=1)
  registry.append(self)
  def read():
   try:
    while True:
     line=self.p.stdout.readline(131073)
     if not line:break
     assert len(line)<=131072 and line.endswith('\n'),'node output bound'
     self.transcript.write(line);self.transcript.flush();self.queue.put(json.loads(line))
   finally:self.queue.put(None)
  self.reader=threading.Thread(target=read,daemon=True);self.reader.start()
  self.ready=self.get(30);assert self.ready['event']=='ready' and self.ready['status']['process_id']==self.p.pid
 def get(self,timeout):
  v=self.queue.get(timeout=timeout)
  if v is None:raise RuntimeError('Node exited: '+self.name)
  return v
 def send(self,op,**fields):
  self.serial+=1;v={'id':self.serial,'op':op,**fields};self.p.stdin.write(json.dumps(v)+'\n');self.p.stdin.flush();return self.serial
 def wait(self,id,timeout=120):
  end=time.monotonic()+timeout
  while id not in self.saved:
   v=self.get(max(.01,end-time.monotonic()));assert 'id'in v,v;self.saved[v['id']]=v
  return self.saved.pop(id)
 def call(self,op,ok=True,**fields):
  r=self.wait(self.send(op,**fields));assert r['ok']==ok,(self.name,op,r)
  return r['result']if ok else r['error']
 def status(self):
  end=time.monotonic()+20
  while True:
   s=self.call('status')
   if not s.get('busy'):return s
   assert time.monotonic()<end;time.sleep(.01)
 def stop(self):
  if self.p.poll()is None:self.call('stop');assert self.p.wait(timeout=20)==0
 def kill(self):
  if self.p.poll()is None:self.p.kill();self.p.wait(timeout=10)
 def cleanup(self):
  self.kill()
  if self.p.stdin:self.p.stdin.close()
  self.reader.join(timeout=2)
  if self.p.stdout:self.p.stdout.close()
  self.err.close();self.transcript.close()

def pack(rows):
 return [{'zone':r['work_object'],'contexts':[{'context':c['context'],'bytes':c['work_object']}for c in r['contexts']if c['context']<2]}for r in rows]

def qualify(corpus,binary,out):
 v=json.loads(corpus.read_text());first=v['first_branch'];winning=v['winning_branch'];now=v['now'];nodes=[];checks=[]
 def check(condition,name):
  assert condition,name;checks.append(name)
 first_hashes=v['canonical_events'][15]['canonical_hashes'];winning_head=v['canonical_events'][-1]['head']
 def state(s,index=first,height=16):
  check(not s['global_full_consensus']and not s['canonical_public_sync']and not s['runtime_go_dependency'],'truthful private scope')
  check(s['indexed_ancestry'],'branch-pinned on-demand ancestry')
  check(s['indexed_catalog']and s['admitted_blocks']>=height+1,'indexed native admission catalog')
  check(s['lazy_etx']and s['lazy_lockups']and s['lazy_accounts']and s['lazy_storage']and s['lazy_qi']and s['canonical_execution_uses_state_database']and s['storage_backend']=='incremental-trie-database'and s['durable_checkpoint']==s['checkpoint'],'native keyed state and durable selected version')
  check(s['height']==height,'Go canonical height')
  expected_head=first_hashes[height]if index==first else winning_head
  check(s['head']==expected_head,'Go canonical head')
  if height:
   row=v['branches'][index]['vectors'][height-1]
   for k in ['evm_root','etx_root','utxo_root']:check(s[k]==row[k],'Go '+k)
 def wait_receipts(node,minimum):
  end=time.monotonic()+30
  while True:
   n=node.call('status')['admitted_blocks']
   if n>=minimum:return n
   assert node.p.poll()is None,'node died during import';assert time.monotonic()<end,'import made no durable progress';time.sleep(.002)
 with tempfile.TemporaryDirectory(prefix='quai-independent-nodes-')as directory:
  root=Path(directory);deny=root/'deny';deny.mkdir();marker=root/'forbidden-tool-attempt'
  for name in ['go','gofmt','protoc']:
   p=deny/name;p.write_text('#!/bin/sh\n: > '+shlex.quote(str(marker))+'\nexit 97\n');p.chmod(0o755)
  env={**os.environ,'PATH':str(deny)+os.pathsep+os.environ['PATH'],'GO_QUAI_DIR':str(root/'absent-go-checkout')}
  def start(name,store=None):return Node(binary,root/(store or name),now,env,out,name,nodes)
  files=[]
  for i in [first,winning]:
   p=root/('branch-'+str(i)+'.json');save(p,pack(v['branches'][i]['vectors']));files.append(p)
  try:
   a=start('source-a');b=start('source-b');receiver=start('receiver')
   state(receiver.status(),height=0)
   check(len({a.p.pid,b.p.pid,receiver.p.pid})==3,'three concurrent independent OS processes')
   for n,p,i in [(a,files[0],first),(b,files[1],winning)]:n.call('import',path=str(p));state(n.status(),index=i)
   identity=receiver.ready['peer_id'];a.call('enroll',peer_id=identity);b.call('enroll',peer_id=identity)
   def sync(node,source,head):return node.call('sync',peer_id=source.ready['peer_id'],address=source.ready['address'],head=head)
   one=sync(receiver,a,first_hashes[-1]);check(one['imported_blocks']==16 and one['requests']==25,'first independent acquisition count');state(receiver.status())
   receiver.stop();receiver=start('receiver-restart-one',store='receiver');check(receiver.ready['peer_id']==identity,'persistent receiver identity after restart');state(receiver.status())
   two=sync(receiver,b,winning_head);check(two['imported_blocks']==8 and two['requests']==13 and two['reorgs']==1,'independent competing-fork native reorg');state(receiver.status(),index=winning)
   receiver.stop();receiver=start('receiver-restart-two',store='receiver');check(receiver.ready['peer_id']==identity,'persistent receiver identity after second restart');state(receiver.status(),index=winning)
   empty=sync(receiver,b,winning_head);check(empty['requests']==0 and empty['imported_blocks']==0,'known head avoids raw requests')
   b_id=b.ready['peer_id'];b.stop();check(sync(receiver,b,winning_head)['requests']==0,'known head succeeds while source process is offline');b=start('source-b-restarted',store='source-b');check(b.ready['peer_id']==b_id,'persistent source identity');b.call('enroll',peer_id=identity);state(b.status(),index=winning)
   check(sync(receiver,b,winning_head)['requests']==0,'known-head acknowledgement survives source process restart')
   before=receiver.status();receiver.call('sync',ok=False,peer_id=a.ready['peer_id'],address='/ip4/8.8.8.8/tcp/30303',head=first_hashes[-1]);check(receiver.status()['checkpoint']==before['checkpoint'],'public address rejected without canonical mutation')
   receiver.call('sync',ok=False,peer_id=a.ready['peer_id'],address=a.ready['address'],head='0x'+'11'*32);check(receiver.status()['checkpoint']==before['checkpoint'],'unavailable remote head cannot mutate canonical state')
   # SIGKILL an actual importing process after durable progress, then resume over P2P.
   crash=start('crash-receiver');a.call('enroll',peer_id=crash.ready['peer_id']);crash_id=crash.ready['peer_id']
   crash.send('sync',peer_id=a.ready['peer_id'],address=a.ready['address'],head=first_hashes[-1]);wait_receipts(crash,4);crash.kill()
   crash=start('crash-receiver-recovered',store='crash-receiver');check(crash.ready['peer_id']==crash_id,'persistent identity after SIGKILL');s=crash.status();state(s,height=s['height']);check(1<=s['height']<=16,'SIGKILL recovered a complete admitted prefix')
   sync(crash,a,first_hashes[-1]);state(crash.status());b.call('enroll',peer_id=crash.ready['peer_id']);after_source_restart=sync(crash,b,winning_head);check(after_source_restart['imported_blocks']==8 and after_source_restart['requests']==13,'restarted source serves missing competing branch over a new connection');state(crash.status(),index=winning);crash.stop()
   # Cancellation reaches a live worker between atomic native publications.
   cancel=start('cancel-receiver');token=cancel.send('import',path=str(files[0]));wait_receipts(cancel,3);requested=cancel.call('cancel');check(requested['requested'],'cancel reaches active native worker');r=cancel.wait(token);check(not r['ok']and r['error']['kind']=='Interrupted','worker acknowledges cancellation')
   s=cancel.status();state(s,height=s['height']);check(1<=s['height']<16,'cancel preserves exact valid prefix');cancel.call('import',path=str(files[0]));state(cancel.status());cancel.stop()
   # Resume a complete pending identity write, but reject corruption and permissive modes.
   key=cancel.directory/'peer.key';key.rename(cancel.directory/'peer.key.pending');resumed=start('identity-pending-recovery',store='cancel-receiver');check(resumed.ready['peer_id']==cancel.ready['peer_id'],'pending identity write resumes without changing peer id');resumed.stop()
   raw=bytearray(key.read_bytes());raw[len(raw)//2]^=1;key.write_bytes(raw)
   def rejected_start(name,path):
    result=subprocess.run([str(binary),'--profile','private-hierarchy-v1','--datadir',str(path),'--now',str(now)],env=env,capture_output=True,text=True,timeout=15)
    (out/(name+'-stderr.log')).write_text(result.stderr);check(result.returncode!=0 and '"event":"ready"'not in result.stdout,name)
   rejected_start('corrupt-peer-key-rejected',cancel.directory)
   key.write_bytes(bytes(raw[:len(raw)//2])+bytes([raw[len(raw)//2]^1])+bytes(raw[len(raw)//2+1:]));key.chmod(0o644);rejected_start('public-readable-peer-key-rejected',cancel.directory);key.chmod(0o600)
   rejected_start('second-writer-rejected',receiver.directory)
   malformed=start('oversized-control');malformed.p.stdin.write('x'*65537+'\n');malformed.p.stdin.flush();message=malformed.get(10);check(not message['ok']and 'byte limit'in message['error']['message'],'oversized command rejected before unbounded allocation');check(malformed.p.wait(timeout=10)!=0,'oversized command closes control input')
   state(receiver.status(),index=winning);state(a.status());state(b.status(),index=winning)
   check(not marker.exists(),'zero Go/protoc process attempts')
   for n in [a,b,receiver]:n.stop()
   result={'passed':True,'private_canonical_execution_uses_state_database':True,'indexed_ancestry':True,'indexed_catalog':True,'lazy_qi':True,'lazy_accounts':True,'lazy_storage':True,'lazy_etx':True,'lazy_lockups':True,'complete_state_images_written':False,'assertions':len(checks),'checks':checks,'unique_peer_blocks_imported':24,'successful_acquisition_requests':one['requests']+two['requests'],'successful_response_payload_bytes':one['response_payload_bytes']+two['response_payload_bytes'],'canonical_reorgs':two['reorgs'],'receiver_clean_restarts':2,'source_clean_restarts':1,'sigkill_recovery_checked':True,'cancellation_prefix_recovery_checked':True,'persistent_identity_checked':True,'separate_os_processes':True,'minimum_concurrent_node_processes':3,'distinct_process_ids':[n.p.pid for n in nodes],'peer_nodes_share_test_process':False,'runtime_go_dependency':False,'forbidden_tool_attempts':0,'head':winning_head,'height':16,'global_full_consensus':False,'canonical_public_sync':False,'public_network':False,'scope':'Frozen early single-zone private profile; separate Rust OS processes with local raw bundles and actual loopback Noise/Yamux acquisition. Go oracle is offline; broad forks, mixed transactions, global coordinator, sustained execution scale and working-resource qualification, receipt/log indexes and production fault/pruning behavior remain unqualified.'}
   save(out/'checks.json',result);return result
  finally:
   for node in nodes:node.cleanup()

def main():
 b=json.loads((ROOT/'results/rust-build.json').read_text())
 for group in ['rust_sources','source_assets']:
  for name,digest in b[group].items():assert sha(ROOT/name)==digest,'Rebuild first: '+name
 corpus=ROOT/'fixtures/protocol/canonical.json';m=json.loads(corpus.with_name('canonical-source.json').read_text());assert m['reference_revision']==b['reference_revision']and m['vectors_sha256']==sha(corpus)and m['profile_sha256']==sha(ROOT/'configs/private-hierarchy-v1.json');assert b['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
 out=ROOT/'results'/('private-processes-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
 with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-network','--features','native-transport','--example','private-node'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
 binary=out/'private-node';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/private-node',binary)
 result=qualify(corpus,binary,out)
 assert sha(Path(__file__))==DRIVER_DIGEST,'Qualification driver changed during execution'
 assert sha(corpus)==m['vectors_sha256'],'Frozen corpus changed during qualification'
 summary={**result,'reference_revision':b['reference_revision'],'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(corpus),'profile_sha256':sha(ROOT/'configs/private-hierarchy-v1.json'),'node_binary_sha256':sha(binary),'driver_sha256':DRIVER_DIGEST,'identity_keys_in_artifacts':False,'source_corpus_not_modified':True,'evidence_files':{p.name:sha(p)for p in out.iterdir()if p.is_file()}}
 save(out/'summary.json',summary);save(ROOT/'results/latest-private-processes.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({k:summary[k]for k in ['passed','assertions','unique_peer_blocks_imported','separate_os_processes','global_full_consensus']},indent=2))
if __name__=='__main__':main()
