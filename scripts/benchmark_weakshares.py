#!/usr/bin/env python3
"""Fixed native Rust binary-precision arithmetic: 512-bit versus 256-bit workspaces."""
import argparse,datetime,json,os,platform,statistics,subprocess,tarfile
from pathlib import Path
from lab import ROOT,sha,save
p=argparse.ArgumentParser();p.add_argument('--baseline',required=True,type=Path);p.add_argument('--baseline-target',required=True,type=Path);p.add_argument('--candidate-target',required=True,type=Path);a=p.parse_args()
base=a.baseline.resolve();sources={'baseline':base,'candidate':ROOT};targets={'baseline':a.baseline_target.resolve(),'candidate':a.candidate_target.resolve()}
frozen=json.loads((base/'results/baseline-source.json').read_text())
for n,h in frozen['files'].items():assert sha(base/n)==h,n
build=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for n,h in build[group].items():assert sha(ROOT/n)==h,n
for n in ['Cargo.lock','crates/quai-consensus/Cargo.toml','crates/quai-consensus/examples/bench-weak-float.rs','fixtures/protocol/weakshares.json','configs/private-hierarchy-v1.json']:assert sha(ROOT/n)==sha(base/n),n
proofs={}
for name in ['stateful','zone-execution','private-branches','admission','confirmation','uncles','weakshares']:
 latest=json.loads((ROOT/'results'/('latest-'+name+'.json')).read_text());path=ROOT/latest['directory']/'summary.json';d=json.loads(path.read_text());assert d['passed']and d['build_sha256']==sha(ROOT/'results/rust-build.json');proofs[str(path.relative_to(ROOT))]=sha(path)
out=ROOT/'results'/('weakshares-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
env={**os.environ,'CARGO_TARGET_DIR':str(targets['baseline']),'CARGO_NET_OFFLINE':'true'}
with(out/'baseline-correctness.log').open('w')as log:subprocess.run(['cargo','test','--release','--locked','--offline','-p','quai-consensus','--test','uncles','--test','weak_float','--test','weakshares','--','--nocapture'],cwd=base,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
correct=(out/'baseline-correctness.log').read_text();assert 'WEAK_FLOAT_CHECKS 5877'in correct and '"checks":760'in correct and correct.count('1 passed; 0 failed')==3
for role in sources:
 env={**os.environ,'CARGO_TARGET_DIR':str(targets[role]),'CARGO_NET_OFFLINE':'true'}
 with(out/(role+'-build.log')).open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','bench-weak-float'],cwd=sources[role],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
raw=[];stable=None
for mode,rounds in [('timing',5),('allocations',3)]:
 for i in range(rounds):
  for role in (['baseline','candidate']if i%2==0 else['candidate','baseline']):
   cmd=[str(targets[role]/'release/examples/bench-weak-float'),str(sources[role]/'fixtures/protocol/weakshares.json')]+(['--allocations']if mode=='allocations'else[])
   load=os.getloadavg();result=json.loads(subprocess.check_output(cmd,cwd=sources[role],text=True))
   same={k:v for k,v in result.items()if k not in ['mode','ns_per_weight','gross_allocated_bytes','allocation_requests']}
   if stable is None:stable=same
   else:assert same==stable
   raw.append({'mode':mode,'round':i,'role':role,'load_average':load,'result':result});save(out/'raw.json',raw)
samples={role:[v['result']['ns_per_weight']for v in raw if v['mode']=='timing'and v['role']==role]for role in sources};medians={role:statistics.median(v)for role,v in samples.items()};metrics={'ns_per_weight':{'samples':samples,'median':medians,'baseline_div_candidate':medians['baseline']/medians['candidate'],'reduction_fraction':1-medians['candidate']/medians['baseline']}}
allocations=[v for v in raw if v['mode']=='allocations'];assert all(v['result']['gross_allocated_bytes']==0 and v['result']['allocation_requests']==0 for v in allocations)
with tarfile.open(out/'baseline-source.tar.gz','w:gz')as archive:
 for n in sorted(frozen['files']):archive.add(base/n,arcname=n,recursive=False)
 archive.add(base/'results/baseline-source.json',arcname='results/baseline-source.json')
for n,h in frozen['files'].items():assert sha(base/n)==h,n
for n,h in build['rust_sources'].items():assert sha(ROOT/n)==h,n
report={'passed':True,'reference_revision':build['reference_revision'],'candidate_build_sha256':sha(ROOT/'results/rust-build.json'),'sequential_runs':True,'timing_rounds':5,'allocation_rounds':3,'timing_and_allocation_separate_processes':True,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'workload':stable,'metrics':metrics,'allocation_requests_observed_per_timed_region':0,'gross_allocated_bytes_observed_per_timed_region':0,'correctness_reports':proofs,'baseline_correctness_sha256':sha(out/'baseline-correctness.log'),'baseline_archive_sha256':sha(out/'baseline-source.tar.gz'),'harness_sha256':sha(ROOT/'crates/quai-consensus/examples/bench-weak-float.rs'),'driver_sha256':sha(Path(__file__).resolve()),'corpus_sha256':sha(ROOT/'fixtures/protocol/weakshares.json'),'binaries':{role:sha(targets[role]/'release/examples/bench-weak-float')for role in sources},'sources':{role:{str(p.relative_to(src)):sha(p)for p in sorted((src/'crates').rglob('*.rs'))}for role,src in sources.items()},'full_consensus':False,'canonical_sync':False,'scope':'Pure weak-share binary-precision arithmetic only: 5875 Go-qualified deficit/distance inputs repeated 16 times per process; exact checksum and per-input warmup comparison. Parsing/setup excluded. Timing and Rust allocation sampling are separate. Both workspaces have zero observed heap requests in the sampled region. Optimizes bounded intermediate storage from 512 to 256 bits; no Go speed comparison, block replay, proof, storage, RPC, node TPS or network-sync performance claim. Retained memory and peak RSS are unmeasured.'}
save(out/'summary.json',report);save(ROOT/'results/latest-weakshares-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'metrics':metrics,'allocation_requests':0},indent=2))
