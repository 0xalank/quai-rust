#!/usr/bin/env python3
"""Controlled independent-process checks for the genesis-only all-ETX contract."""
import datetime,json,os,shutil,signal,subprocess,tempfile
from pathlib import Path
from lab import ROOT,sha,save
from verify_private_processes import Node,pack
p=ROOT/'results/rust-build.json'
build_sha=sha(p) if p.is_file() else None
source_inputs={str(p.relative_to(ROOT)):sha(p) for p in (ROOT/'crates').rglob('*.rs')}
profile=ROOT/'configs/private-execution-v3.json';profile_sha=sha(profile)
out=ROOT/'results'/('etx-runtime-processes-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir()
shutil.copy2(Path(__file__),out/'qualification-driver.py')
helper_sha=sha(ROOT/'scripts/verify_private_processes.py')
binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/private-node'
assert binary.is_file();node=out/'private-node';shutil.copy2(binary,node)
corpus=ROOT/'fixtures/protocol/canonical.json';v=json.loads(corpus.read_text());rows=v['branches'][v['first_branch']]['vectors'];heads=v['canonical_events'][15]['canonical_hashes'];checks=[];nodes=[]
def check(ok,name):assert ok,name;checks.append(name)
def selected(s):return{k:s[k] for k in ['head','height','checkpoint','evm_root','etx_root','utxo_root','admitted_blocks']}
with tempfile.TemporaryDirectory(prefix='quai-etx-runtime-processes-') as scratch:
 scratch=Path(scratch);packfile=scratch/'first.json';packfile.write_text(json.dumps(pack(rows[:16])))
 shims=scratch/'deny';shims.mkdir();marker=scratch/'forbidden'
 for command in ['go','gofmt','protoc']:
  path=shims/command;path.write_text('#!/bin/sh\n: > '+str(marker)+'\nexit 97\n');path.chmod(0o755)
 env={**os.environ,'PATH':str(shims)+os.pathsep+os.environ['PATH'],'GO_QUAI_DIR':str(scratch/'missing-go')}
 try:
  source=Node(node,scratch/'source',v['now'],env,out,'source',nodes,profile='private-execution-v3')
  receiver=Node(node,scratch/'receiver',v['now'],env,out,'receiver',nodes,profile='private-execution-v3')
  check(source.p.pid!=receiver.p.pid,'independent native source and receiver processes')
  check(source.ready['status']['profile']==receiver.ready['status']['profile']=='private-execution-v3','both processes use the new explicit contract')
  check(source.ready['status']['all_defined_etx_execution'] and receiver.ready['status']['all_defined_etx_execution'],'actual node reports all-defined-ETX runtime dispatch')
  source.call('import',path=str(packfile));s=source.status();check(s['height']==16 and s['head']==heads[16],'source admits the actual-Go genesis-linked branch')
  identity=receiver.ready['peer_id'];source.call('enroll',peer_id=identity);receiver.call('enroll',peer_id=source.ready['peer_id'])
  receiver.call('follow',peer_id=source.ready['peer_id'],address=source.ready['address'],head=heads[8],height=8,window_blocks=4)
  middle=receiver.status();check(middle['height']==8 and middle['head']==heads[8],'receiver acquires eight blocks through native peer transport')
  receiver.p.send_signal(signal.SIGKILL);receiver.p.wait(timeout=10);receiver.cleanup();nodes.remove(receiver)
  receiver=Node(node,scratch/'receiver',v['now'],env,out,'receiver-restarted',nodes,profile='private-execution-v3')
  check(receiver.ready['peer_id']==identity,'SIGKILL recovery preserves the peer identity')
  check(selected(receiver.status())==selected(middle),'SIGKILL recovery preserves the complete selected generation')
  receiver.call('enroll',peer_id=source.ready['peer_id'])
  receiver.call('follow',peer_id=source.ready['peer_id'],address=source.ready['address'],head=heads[16],height=16,window_blocks=4)
  final=receiver.status();check(selected(final)==selected(s),'receiver reaches exact source heads and execution roots after resuming')
  check(final['evm_root']==rows[15]['evm_root'] and final['etx_root']==rows[15]['etx_root'],'selected execution roots equal the pinned Go branch')
  before=selected(final);known=receiver.call('follow',peer_id=source.ready['peer_id'],address='/ip4/127.0.0.1/tcp/1',head=heads[16],height=16,window_blocks=4)
  check(known['requests']==0 and known['windows']==0,'known head uses no peer I/O')
  check(selected(receiver.status())==before,'known-head following preserves the selected generation')
  receiver.stop();receiver.cleanup();nodes.remove(receiver)
  wrong=subprocess.run([str(node),'--profile','private-execution-v2','--datadir',str(scratch/'receiver'),'--now',str(v['now'])],env=env,capture_output=True,text=True,timeout=10)
  check(wrong.returncode!=0,'the earlier runtime cannot open the new contract datadir')
  receiver=Node(node,scratch/'receiver',v['now'],env,out,'receiver-after-wrong-profile',nodes,profile='private-execution-v3')
  check(selected(receiver.status())==before,'wrong-profile startup leaves native selected state unchanged')
  check(not final['global_full_consensus'] and not final['canonical_public_sync'],'process output retains the full-consensus/public-sync limits')
  source.stop();receiver.stop()
  check(not marker.exists(),'Go and protoc were never invoked')
  save(out/'selected-generations.json',{'source':selected(s),'before_kill':selected(middle),'receiver':before})
 finally:
  for item in nodes:item.cleanup()
for n,h in source_inputs.items():assert sha(ROOT/n)==h,n
assert sha(profile)==profile_sha and sha(ROOT/'scripts/verify_private_processes.py')==helper_sha
q={'passed':True,'build_sha256':build_sha,'source_inputs':source_inputs,'profile_sha256':profile_sha,'node_binary_sha256':sha(node),'assertions':len(checks),'checks':checks,'genesis_linked_blocks':16,'separate_os_processes':True,'source_datadir_imported':False,'checkpoint_imported':False,'runtime_go_dependency':False,'full_consensus':False,'canonical_public_sync':False,'scope':'Actual all-ETX runtime/transport/genesis/recovery path on the frozen early Go branch. Seven-kind activated dispatch has separate supplied-history unit evidence; this does not add activated canonical ancestry.','evidence_files':{p.name:sha(p) for p in out.iterdir() if p.is_file()}}
save(out/'summary.json',q);save(ROOT/'results/latest-etx-runtime-processes.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'assertions':len(checks),'directory':str(out.relative_to(ROOT))},indent=2))
