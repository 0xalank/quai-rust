#!/usr/bin/env python3
"""Paired fixed offline workloads against an archived, already-verified Rust baseline.
Run only after correctness checks pass and other compilation/tests have stopped.
"""
import argparse,datetime,hashlib,json,os,platform,statistics,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
p=argparse.ArgumentParser();p.add_argument('--baseline',required=True,type=Path);p.add_argument('--baseline-target',required=True,type=Path);p.add_argument('--candidate-target',required=True,type=Path);p.add_argument('--rounds',type=int,default=5);a=p.parse_args();assert 3<=a.rounds<=20
baseline=a.baseline.resolve();targets={'baseline':a.baseline_target.resolve(),'candidate':a.candidate_target.resolve()};sources={'baseline':baseline,'candidate':ROOT};build=json.loads((ROOT/'results/rust-build.json').read_text());old=json.loads((baseline/'results/baseline-build.json').read_text())
for source,metadata in [(ROOT,build),(baseline,old)]:
 assert metadata['reference_revision']=='33db82f2891b267ba898ca38bd2dd8bf75cbb072'
 for name,digest in metadata['rust_sources'].items():assert sha(source/name)==digest,name
assert sha(baseline/'Cargo.lock')==sha(ROOT/'Cargo.lock')
for name in ['bench-consensus-state.rs','bench-signed-settlement.rs','bench-state-commit.rs']:assert sha(baseline/'crates/quai-execution/examples'/name)==sha(ROOT/'crates/quai-execution/examples'/name)
latest=json.loads((ROOT/'results/latest-stateful.json').read_text());correctness=json.loads((ROOT/latest['directory']/'summary.json').read_text());assert correctness['passed']and correctness['build_sha256']==sha(ROOT/'results/rust-build.json')
out=ROOT/'results'/('consensus-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
for role in ['baseline','candidate']:
 env={**os.environ,'CARGO_TARGET_DIR':str(targets[role]),'CARGO_NET_OFFLINE':'true'}
 with (out/(role+'-build.log')).open('w')as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-execution','--example','bench-consensus-state','--example','bench-signed-settlement','--example','bench-state-commit'],cwd=sources[role],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
raw=[]
for round_number in range(a.rounds):
 for role in (['baseline','candidate']if round_number%2==0 else['candidate','baseline']):
  for name in ['bench-consensus-state','bench-signed-settlement','bench-state-commit']:
   binary=targets[role]/'release/examples'/name;args=[str(binary)]
   if name=='bench-signed-settlement':args.append(str(ROOT/'fixtures/protocol/settlement.json'))
   load=os.getloadavg();result=json.loads(subprocess.check_output(args,cwd=sources[role],text=True));raw.append({'round':round_number,'role':role,'workload':name,'load_average':load,'result':result})
   (out/'raw.json').write_text(json.dumps(raw,indent=2)+'\n')
# Consensus results, workload sizes and checksums must agree in every run.
for name in ['bench-consensus-state','bench-signed-settlement','bench-state-commit']:
 reference=None
 for row in [v for v in raw if v['workload']==name]:
  r=row['result'];stable={k:v for k,v in r.items()if not k.endswith('_ns')}
  if name=='bench-signed-settlement':stable['cases']=[{k:v for k,v in c.items()if k!='ns_per_operation'}for c in stable['cases']]
  if reference is None:reference=stable
  else:assert stable==reference,(name,row['role'],row['round'])
metrics={}
for metric,workload in [('clone_ns','bench-consensus-state'),('execution_ns','bench-consensus-state'),('root_ns','bench-consensus-state'),('dirty_commit_ns','bench-state-commit')]:
 samples={role:[r['result'][metric]for r in raw if r['role']==role and r['workload']==workload]for role in sources};medians={role:statistics.median(v)for role,v in samples.items()};metrics[metric]={'samples_ns':samples,'median_ns':medians,'speedup':medians['baseline']/medians['candidate']}
for i,label in enumerate(['signed_transfer','signed_storage','signed_creation']):
 samples={role:[r['result']['cases'][i]['ns_per_operation']for r in raw if r['role']==role and r['workload']=='bench-signed-settlement']for role in sources};medians={role:statistics.median(v)for role,v in samples.items()};metrics[label]={'samples_ns':samples,'median_ns':medians,'speedup':medians['baseline']/medians['candidate']}
report={'passed':True,'reference_revision':build['reference_revision'],'rounds':a.rounds,'shared_host':True,'sequential_timing':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'candidate_build_sha256':sha(ROOT/'results/rust-build.json'),'baseline_build_sha256':sha(baseline/'results/baseline-build.json'),'correctness_summary_sha256':sha(ROOT/latest['directory']/'summary.json'),'fixture_sha256':sha(ROOT/'fixtures/protocol/settlement.json'),'harness_sha256':{n:sha(ROOT/'crates/quai-execution/examples'/n)for n in ['bench-consensus-state.rs','bench-signed-settlement.rs','bench-state-commit.rs']},'binaries':{role:{n:sha(targets[role]/'release/examples'/n)for n in ['bench-consensus-state','bench-signed-settlement','bench-state-commit']}for role in sources},'source_sha256':{role:{str(f.relative_to(source)):sha(f)for f in sorted((source/'crates').rglob('*.rs'))}for role,source in sources.items()},'metrics':metrics,'full_consensus':False,'canonical_sync':False,'scope':'Prior Rust versus current Rust on fixed offline state and signed-settlement workloads. Timers exclude setup, parsing and root verification; full roots/gas checksums are compared before accepting timing. No network TPS, end-to-end sync, peak-memory or Go performance claim.'};(out/'summary.json').write_text(json.dumps(report,indent=2)+'\n');(ROOT/'results/latest-consensus-performance.json').write_text(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT))},indent=2)+'\n');print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'speedups':{k:round(v['speedup'],3)for k,v in metrics.items()}},indent=2))
