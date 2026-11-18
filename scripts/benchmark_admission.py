#!/usr/bin/env python3
"""Fixed sequential distinct-header preparation, Rust before/after proof reuse."""
import argparse,datetime,json,os,platform,statistics,subprocess,tarfile
from pathlib import Path
from lab import ROOT,sha,save
p=argparse.ArgumentParser();p.add_argument('--baseline',type=Path,required=True);p.add_argument('--baseline-target',type=Path,required=True);p.add_argument('--candidate-target',type=Path,required=True);a=p.parse_args()
base=a.baseline.resolve();sources={'baseline':base,'candidate':ROOT};targets={'baseline':a.baseline_target.resolve(),'candidate':a.candidate_target.resolve()};old=json.loads((base/'results/baseline-source.json').read_text())
for n,h in old['files'].items():assert sha(base/n)==h,n
assert '1 passed; 0 failed' in (base/'results/admission-baseline-tests.log').read_text()
b=json.loads((ROOT/'results/rust-build.json').read_text());assert b['reference_revision']==old['reference_revision']
for group in ['rust_sources','source_assets']:
 for n,h in b[group].items():assert sha(ROOT/n)==h,n
for n in ['Cargo.lock','configs/private-admission-v1.json','fixtures/protocol/admission.json','crates/quai-consensus/examples/bench-admission.rs']:assert sha(ROOT/n)==sha(base/n),n
latest=json.loads((ROOT/'results/latest-admission.json').read_text());proof=ROOT/latest['directory']/'summary.json';d=json.loads(proof.read_text());assert d['passed']and d['build_sha256']==sha(ROOT/'results/rust-build.json')
out=ROOT/'results'/('admission-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
for role in sources:
 env={**os.environ,'CARGO_TARGET_DIR':str(targets[role]),'CARGO_NET_OFFLINE':'true'}
 with(out/(role+'-build.log')).open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','bench-admission'],cwd=sources[role],env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
raw=[];stable=None
for number in range(5):
 for role in (['baseline','candidate']if number%2==0 else['candidate','baseline']):
  load=os.getloadavg();result=json.loads(subprocess.check_output([str(targets[role]/'release/examples/bench-admission'),str(ROOT/'fixtures/protocol/admission.json')],cwd=sources[role],text=True));output={k:v for k,v in result.items()if k!='ns_per_header'}
  if stable is None:stable=output
  else:assert output==stable,(number,role)
  raw.append({'round':number,'role':role,'load_average':load,'result':result});save(out/'raw.json',raw)
samples={role:[r['result']['ns_per_header']for r in raw if r['role']==role]for role in sources};medians={role:statistics.median(v)for role,v in samples.items()}
with tarfile.open(out/'baseline-source.tar.gz','w:gz')as archive:
 for name in sorted(old['files']):archive.add(base/name,arcname=name,recursive=False)
 archive.add(base/'results/baseline-source.json',arcname='results/baseline-source.json')
for n,h in old['files'].items():assert sha(base/n)==h
for n,h in b['rust_sources'].items():assert sha(ROOT/n)==h
report={'passed':True,'reference_revision':b['reference_revision'],'rounds':5,'sequential_timing':True,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'workload':stable,'samples_ns':samples,'median_ns':medians,'speedup':medians['baseline']/medians['candidate'],'candidate_build_sha256':sha(ROOT/'results/rust-build.json'),'correctness_report':str(proof.relative_to(ROOT)),'correctness_sha256':sha(proof),'harness_sha256':sha(ROOT/'crates/quai-consensus/examples/bench-admission.rs'),'driver_sha256':sha(Path(__file__).resolve()),'baseline_archive_sha256':sha(out/'baseline-source.tar.gz'),'binaries':{role:sha(targets[role]/'release/examples/bench-admission')for role in sources},'sources':{role:{str(f.relative_to(source)):sha(f)for f in sorted((source/'crates').rglob('*.rs'))}for role,source in sources.items()},'full_proof_cache_limits':{'entries':512,'logical_bytes':4194304,'separate_from_inclusion_work_memo':True,'rss_or_allocator_measurement':False},'full_consensus':False,'canonical_sync':False,'scope':'One sequence of sixteen distinct local-zone native header/body/manifest/PCRC preparations. Previously authenticated parents and repeated checks of the candidate reuse full proof results; all policy/clock/commitment checks still run. Epoch construction/input decoding excluded. No execution, persistence, RPC, network, complete sync or peak-memory speedup is established.'}
save(out/'summary.json',report);save(ROOT/'results/latest-admission-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'median_ns':medians,'speedup':report['speedup']},indent=2))
