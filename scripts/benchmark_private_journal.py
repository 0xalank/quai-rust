#!/usr/bin/env python3
"""Fixed Rust/Rust journal timings and separate gross-allocation sampling."""
import argparse, datetime, json, os, platform, statistics, subprocess, tarfile
from pathlib import Path
from lab import ROOT, sha, save
p=argparse.ArgumentParser();p.add_argument('--baseline',required=True,type=Path);p.add_argument('--baseline-target',required=True,type=Path);p.add_argument('--candidate-target',required=True,type=Path);a=p.parse_args()
baseline=a.baseline.resolve();sources={'baseline':baseline,'candidate':ROOT};targets={'baseline':a.baseline_target.resolve(),'candidate':a.candidate_target.resolve()}
old=json.loads((baseline/'results/baseline-source.json').read_text())
for n,h in old['files'].items():assert sha(baseline/n)==h,n
assert '15 passed; 0 failed' in (baseline/'results/durable-baseline-store-tests.log').read_text()
assert '3 passed; 0 failed' in (baseline/'results/durable-branches-tests.log').read_text()
build=json.loads((ROOT/'results/rust-build.json').read_text())
assert old['reference_revision']==build['reference_revision']
for group in ['rust_sources','source_assets']:
 for n,h in build[group].items():assert sha(ROOT/n)==h,n
for n in ['Cargo.lock','crates/quai-store/Cargo.toml','crates/quai-store/examples/bench-private-journal.rs']:
 assert sha(ROOT/n)==sha(baseline/n),n
proofs={}
for component in ['stateful','zone-execution','private-branches']:
 latest=json.loads((ROOT/'results'/('latest-'+component+'.json')).read_text());path=ROOT/latest['directory']/'summary.json';d=json.loads(path.read_text());assert d['passed']and d['build_sha256']==sha(ROOT/'results/rust-build.json');proofs[str(path.relative_to(ROOT))]=sha(path)
out=ROOT/'results'/('journal-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
for role in sources:
 env={**os.environ,'CARGO_TARGET_DIR':str(targets[role]),'CARGO_NET_OFFLINE':'true'}
 with (out/(role+'-build.log')).open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-store','--example','bench-private-journal'],cwd=sources[role],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
raw=[];stable=None
for mode,rounds in [('timing',5),('allocations',3)]:
 for number in range(rounds):
  for role in (['baseline','candidate']if number%2==0 else['candidate','baseline']):
   cmd=[str(targets[role]/'release/examples/bench-private-journal')]+(['--allocations']if mode=='allocations'else[])
   load=os.getloadavg();result=json.loads(subprocess.check_output(cmd,cwd=sources[role],text=True));output={k:v for k,v in result.items()if k not in ['mode','ns_per_switch','gross_allocated_bytes_per_switch','allocation_requests_per_switch']}
   if stable is None:stable=output
   else:assert output==stable,(mode,role,number)
   raw.append({'mode':mode,'round':number,'role':role,'load_average':load,'result':result});save(out/'raw.json',raw)
metrics={}
for name,mode in [('ns_per_switch','timing'),('gross_allocated_bytes_per_switch','allocations'),('allocation_requests_per_switch','allocations')]:
 samples={role:[v['result'][name]for v in raw if v['mode']==mode and v['role']==role]for role in sources};medians={role:statistics.median(v)for role,v in samples.items()};metrics[name]={'samples':samples,'median':medians,'baseline_div_candidate':medians['baseline']/medians['candidate'],'reduction_fraction':1-medians['candidate']/medians['baseline']}
with tarfile.open(out/'baseline-source.tar.gz','w:gz')as archive:
 for name in sorted(old['files']):archive.add(baseline/name,arcname=name,recursive=False)
 archive.add(baseline/'results/baseline-source.json',arcname='results/baseline-source.json')
for n,h in old['files'].items():assert sha(baseline/n)==h,n
for n,h in build['rust_sources'].items():assert sha(ROOT/n)==h,n
report={'passed':True,'reference_revision':build['reference_revision'],'shared_host':True,'sequential_runs':True,'timing_rounds':5,'allocation_rounds':3,'timing_and_allocation_separate_processes':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'workload':stable,'metrics':metrics,'candidate_build_sha256':sha(ROOT/'results/rust-build.json'),'correctness_reports':proofs,'harness_sha256':sha(ROOT/'crates/quai-store/examples/bench-private-journal.rs'),'driver_sha256':sha(Path(__file__).resolve()),'baseline_archive_sha256':sha(out/'baseline-source.tar.gz'),'binaries':{role:sha(targets[role]/'release/examples/bench-private-journal')for role in sources},'sources':{role:{str(f.relative_to(source)):sha(f)for f in sorted((source/'crates').rglob('*.rs'))}for role,source in sources.items()},'full_consensus':False,'canonical_sync':False,'scope':'Two private journal tips at height 63 sharing 48 checkpoints; 256 KiB opaque payloads; eight selected-head switches per process. Includes fresh full content checksums, traversal, head fsync/rename; filesystem page cache is warm. Gross Rust allocator requests sampled separately, reallocations count full requested new size. Does not measure retained memory, peak RSS, cold-device I/O, the complete branch wrapper, node TPS or end-to-end sync.'}
save(out/'summary.json',report);save(ROOT/'results/latest-journal-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))})
print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'metrics':metrics},indent=2))
