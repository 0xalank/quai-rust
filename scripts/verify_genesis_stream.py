#!/usr/bin/env python3
"""Source-bound native canonical replay of an explicit frozen Go genesis stream."""
import argparse,datetime,gzip,hashlib,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,save,sha
p=argparse.ArgumentParser();p.add_argument('--oracle',type=Path,required=True);a=p.parse_args();oracle=a.oracle.resolve();m=json.loads((oracle/'source.json').read_text());b=json.loads((ROOT/'results/rust-build.json').read_text());build=sha(ROOT/'results/rust-build.json');driver=sha(Path(__file__))
for group in ['rust_sources','source_assets']:
 for n,h in b[group].items():assert sha(ROOT/n)==h,'Rebuild first: '+n
assert m['passed']and m['reference_revision']==b['reference_revision']and m['validated_from_genesis']and not m['checkpoint_imported']and not m['initial_allocations']and m['profile_sha256']==sha(ROOT/'configs/private-execution-v2.json')and m['driver_sha256']==sha(ROOT/'scripts/gen_genesis_stream.py')
for n,h in m['oracle_sources'].items():assert sha(ROOT/n)==h,n
corpus=oracle/'blocks.jsonl.gz';assert sha(corpus)==m['compressed_stream_sha256'];out=ROOT/'results'/('genesis-replay-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir();shutil.copy2(Path(__file__),out/'qualification-driver.py');shutil.copy2(oracle/'source.json',out/'oracle-source.json')
with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-genesis-stream'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'check-genesis-stream';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-genesis-stream',binary)
with tempfile.TemporaryDirectory(prefix='quai-genesis-replay-')as directory:
 directory=Path(directory);deny=directory/'deny';deny.mkdir();marker=directory/'forbidden';data=directory/'data'
 for name in ['go','gofmt','protoc']:
  path=deny/name;path.write_text('#!/bin/sh\n: > '+str(marker)+'\nexit 97\n');path.chmod(0o755)
 env={**os.environ,'PATH':str(deny)+os.pathsep+os.environ['PATH'],'GO_QUAI_DIR':str(directory/'absent-go')};runs=[]
 for role in ['fresh','known-history-reopen']:
  raw=hashlib.sha256()
  with gzip.open(corpus,'rb')as source,(out/(role+'.log')).open('wb')as log:
   proc=subprocess.Popen([str(binary),str(data)],stdin=subprocess.PIPE,stdout=log,stderr=subprocess.STDOUT,env=env,cwd=ROOT)
   try:
    while chunk:=source.read(1<<20):raw.update(chunk);proc.stdin.write(chunk)
    proc.stdin.close();assert proc.wait(timeout=3600)==0,(out/(role+'.log')).read_text()[-3000:]
   finally:
    if proc.poll()is None:proc.kill();proc.wait()
  assert raw.hexdigest()==m['raw_stream_sha256'];rows=[json.loads(n)for n in(out/(role+'.log')).read_text().splitlines()];q=rows[-1];assert q['kind']=='result'and q['passed']and q['blocks']==m['blocks']and q['head']==m['head']and q['signed_quai_transactions']==m['signed_quai_transactions']and q['genesis_to_tip_local_receipts']and not q['checkpoint_imported']and not q['full_consensus']and not q['canonical_public_sync']and not q['runtime_go_dependency'];runs.append(q)
 assert runs[0]['native_imports_this_run']==m['blocks']and runs[0]['resumed_from_height']==0 and runs[1]['native_imports_this_run']==0 and runs[1]['resumed_from_height']==m['blocks']and not marker.exists()
assert driver==sha(Path(__file__))
report={'passed':True,'build_sha256':build,'reference_revision':b['reference_revision'],'blocks':m['blocks'],'checks':sum(q['checks']for q in runs),'fresh_checks':runs[0]['checks'],'known_history_checks':runs[1]['checks'],'writer_reopens':runs[0]['writer_reopens']+runs[1]['writer_reopens'],'head':m['head'],'signed_quai_transactions':m['signed_quai_transactions'],'context_views':runs[0]['context_views'],'runs':runs,'source_oracle_directory':str(oracle.relative_to(ROOT))if oracle.is_relative_to(ROOT)else str(oracle),'source_oracle_sha256':sha(oracle/'source.json'),'compressed_stream_sha256':sha(corpus),'raw_stream_sha256':m['raw_stream_sha256'],'checker_artifact':str(binary.relative_to(ROOT)),'checker_sha256':sha(binary),'qualification_driver_sha256':driver,'profile_sha256':m['profile_sha256'],'checkpoint_imported':False,'genesis_to_tip_local_receipts':True,'genesis_to_activation_qualified':m['blocks']>259200 and m['signed_quai_transactions']>0,'runtime_go_dependency':False,'go_commands_attempted':0,'full_consensus':False,'canonical_public_sync':False,'public_network':False,'scope':'Exact native local canonical import from the declared empty genesis through every supplied Go block, all selected roots and context witnesses; repeated writer recovery and receipt-pinned known-history replay. This is an experimental private profile, not full fork/ETX/nonlocal/expansion/public qualification.','evidence_files':{x.name:sha(x)for x in out.iterdir()if x.is_file()}};save(out/'summary.json',report);save(ROOT/'results/latest-genesis-replay.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({k:report[k]for k in ['passed','blocks','checks','writer_reopens','signed_quai_transactions','genesis_to_activation_qualified']},indent=2))
