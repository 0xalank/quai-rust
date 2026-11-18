#!/usr/bin/env python3
"""Rebind replay evidence only across an exact checker and unchanged inputs."""
import argparse,datetime,json,os,shutil,subprocess,sys
from pathlib import Path
from lab import ROOT,sha,save
p=argparse.ArgumentParser();p.add_argument('--oracle',type=Path,required=True);p.add_argument('--previous',type=Path,required=True);p.add_argument('--previous-build',type=Path,required=True);a=p.parse_args();oracle=a.oracle.resolve();previous=a.previous.resolve();prior_build=a.previous_build.resolve();old=json.loads(previous.read_text());before=json.loads(prior_build.read_text());build=json.loads((ROOT/'results/rust-build.json').read_text());driver=sha(Path(__file__))
assert old['passed']and old['build_sha256']==sha(prior_build)and old['blocks']==8192 and not old['genesis_to_activation_qualified']
for n,h in old['evidence_files'].items():assert sha(previous.parent/n)==h,n
assert old['checker_sha256']==sha(ROOT/old['checker_artifact'])and old['qualification_driver_sha256']==sha(ROOT/'scripts/verify_genesis_stream.py')
assert before['reference_revision']==build['reference_revision']and before['cargo_lock_sha256']==build['cargo_lock_sha256']and before['source_assets']==build['source_assets']
for group in ['rust_sources','source_assets']:
 for name,h in build[group].items():assert sha(ROOT/name)==h,name
assert set(before['rust_sources'])==set(build['rust_sources']);different=[n for n,h in before['rust_sources'].items()if build['rust_sources'][n]!=h]
assert different==['crates/quai-network/examples/private-node.rs'],different
m=json.loads((oracle/'source.json').read_text());assert old['source_oracle_sha256']==sha(oracle/'source.json')and old['compressed_stream_sha256']==sha(oracle/'blocks.jsonl.gz')and old['raw_stream_sha256']==m['raw_stream_sha256']and old['profile_sha256']==sha(ROOT/'configs/private-execution-v2.json')
for n,h in m['oracle_sources'].items():assert sha(ROOT/n)==h,n
out=ROOT/'results'/('genesis-replay-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir()
with(out/'current-checker-build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-genesis-stream'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
current=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-genesis-stream'
if sha(current)!=old['checker_sha256']:
 print('Checker executable changed; running a fresh native replay.',flush=True)
 subprocess.run([sys.executable,'-B',str(ROOT/'scripts/verify_genesis_stream.py'),'--oracle',str(oracle)],cwd=ROOT,check=True);raise SystemExit(0)
for name in old['evidence_files']:shutil.copy2(previous.parent/name,out/name)
shutil.copy2(current,out/'current-check-genesis-stream');shutil.copy2(prior_build,out/'prior-rust-build.json');shutil.copy2(previous,out/'prior-summary.json');shutil.copy2(Path(__file__),out/'exact-reuse-driver.py')
report={**old,'build_sha256':sha(ROOT/'results/rust-build.json'),'checker_artifact':str((out/'current-check-genesis-stream').relative_to(ROOT)),'source_oracle_directory':str(oracle.relative_to(ROOT))if oracle.is_relative_to(ROOT)else str(oracle),'reused_exact_checker_and_inputs':True,'replay_executed_again_this_build':False,'prior_build_sha256':sha(prior_build),'prior_replay_summary_sha256':sha(previous),'changed_rust_sources':different,'exact_reuse_driver_sha256':driver,'scope':old['scope']+' Replay outputs are reused across a change confined to the network node example: a fresh build yields a byte-identical consensus checker, and all oracle, profile, input and original-driver hashes match. The changed node requires separate live process qualification.','evidence_files':{n.name:sha(n)for n in out.iterdir()if n.is_file()}}
assert driver==sha(Path(__file__));save(out/'summary.json',report);save(ROOT/'results/latest-genesis-replay.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'blocks':report['blocks'],'checks':report['checks'],'reused_exact_checker_and_inputs':True,'build_sha256':report['build_sha256']},indent=2))
