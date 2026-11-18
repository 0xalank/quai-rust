#!/usr/bin/env python3
"""Frozen paired root-algorithm experiment, including a separate cold-root guard."""
import argparse,datetime,hashlib,json,os,platform,re,shutil,statistics,subprocess,tarfile
from pathlib import Path
from lab import ROOT,save,sha
p=argparse.ArgumentParser();p.add_argument('--baseline',type=Path,required=True);p.add_argument('--baseline-target',type=Path,required=True);p.add_argument('--candidate-target',type=Path,required=True);p.add_argument('--rounds',type=int,default=5);a=p.parse_args();assert 3<=a.rounds<=20
baseline=a.baseline.resolve();targets={'baseline':a.baseline_target.resolve(),'candidate':a.candidate_target.resolve()};assert targets['baseline']!=targets['candidate'];driver_digest=sha(Path(__file__));sources={'baseline':baseline,'candidate':ROOT}
build=json.loads((ROOT/'results/rust-build.json').read_text())
assert build['reference_revision']=='33db82f2891b267ba898ca38bd2dd8bf75cbb072'
for group in ['rust_sources','source_assets']:
 for name,digest in build[group].items():assert sha(ROOT/name)==digest,name
assert sha(baseline/'Cargo.lock')==sha(ROOT/'Cargo.lock')==build['cargo_lock_sha256']
for name in build['source_assets']:assert sha(baseline/name)==sha(ROOT/name),name
assert {str(p.relative_to(baseline))for p in (baseline/'crates').rglob('*.rs')}==set(build['rust_sources'])
fixtures={str(p.relative_to(ROOT)):sha(p)for p in (ROOT/'fixtures/protocol').glob('*.json')}
for name,digest in fixtures.items():assert sha(baseline/name)==digest,name
changed=[name for name in build['rust_sources']if sha(baseline/name)!=sha(ROOT/name)]
assert changed==['crates/quai-execution/src/evm/state.rs'],changed
for name in ['stateful','state-db']:
 q=json.loads((ROOT/'results'/('latest-'+name+'.json')).read_text());q=json.loads((ROOT/q['directory']/'summary.json').read_text());assert q['passed']and q['build_sha256']==sha(ROOT/'results/rust-build.json')
harnesses=['bench-consensus-state','bench-signed-settlement','bench-state-commit','bench-cold-root','bench-initial-root']
for name in harnesses:assert sha(baseline/'crates/quai-execution/examples'/(name+'.rs'))==sha(ROOT/'crates/quai-execution/examples'/(name+'.rs'))
out=ROOT/'results'/('incremental-state-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True);shutil.copy2(Path(__file__),out/'benchmark-driver.py')
environments={role:{**os.environ,'CARGO_TARGET_DIR':str(target),'CARGO_NET_OFFLINE':'true'}for role,target in targets.items()}
with(out/'baseline-tests.log').open('w')as log:
 subprocess.run(['cargo','test','--release','--locked','--offline','-p','quai-execution'],cwd=baseline,env=environments['baseline'],stdout=log,stderr=subprocess.STDOUT,check=True)
tests=(out/'baseline-tests.log').read_text();assert 'test result: FAILED'not in tests and 'incremental_validation_preserves_aggregate_slot_limits_after_failed_roots ... ok'in tests
binary_paths={}
for role,source in sources.items():
 with(out/(role+'-build.log')).open('w')as log:
  subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-execution',*[v for name in harnesses for v in ['--example',name]]],cwd=source,env=environments[role],stdout=log,stderr=subprocess.STDOUT,check=True)
 binary_paths[role]={};(out/role).mkdir()
 for name in harnesses:
  destination=out/role/name;shutil.copy2(targets[role]/'release/examples'/name,destination);binary_paths[role][name]=destination
assert sha(Path(__file__))==driver_digest,'Benchmark driver changed after launch'
for name in harnesses:assert sha(binary_paths['baseline'][name])!=sha(binary_paths['candidate'][name]),'Both roles reused identical executable: '+name
# Compilers/tests have completed before any paired sample is collected.
raw=[]
for round_number in range(a.rounds):
 for role in (['baseline','candidate']if round_number%2==0 else['candidate','baseline']):
  for name in harnesses:
   arguments=[str(binary_paths[role][name])]
   if name=='bench-signed-settlement':arguments.append(str(ROOT/'fixtures/protocol/settlement.json'))
   load=os.getloadavg();result=json.loads(subprocess.check_output(arguments,cwd=sources[role],text=True));raw.append({'round':round_number,'role':role,'workload':name,'load_average':load,'result':result});save(out/'raw.json',raw)
for name in harnesses:
 reference=None
 for row in [v for v in raw if v['workload']==name]:
  stable={k:v for k,v in row['result'].items()if not k.endswith('_ns')}
  if name=='bench-signed-settlement':stable['cases']=[{k:v for k,v in c.items()if k!='ns_per_operation'}for c in stable['cases']]
  if reference is None:reference=stable
  else:assert stable==reference,(name,row['role'],row['round'])
metrics={}
for metric,workload in [('clone_ns','bench-consensus-state'),('execution_ns','bench-consensus-state'),('root_ns','bench-consensus-state'),('dirty_commit_ns','bench-state-commit'),('cold_root_ns','bench-cold-root'),('initial_root_ns','bench-initial-root')]:
 samples={role:[r['result'][metric]for r in raw if r['role']==role and r['workload']==workload]for role in sources};medians={role:statistics.median(v)for role,v in samples.items()};metrics[metric]={'samples_ns':samples,'median_ns':medians,'speedup':medians['baseline']/medians['candidate']}
for i,label in enumerate(['signed_transfer','signed_storage','signed_creation']):
 samples={role:[r['result']['cases'][i]['ns_per_operation']for r in raw if r['role']==role and r['workload']=='bench-signed-settlement']for role in sources};medians={role:statistics.median(v)for role,v in samples.items()};metrics[label]={'samples_ns':samples,'median_ns':medians,'speedup':medians['baseline']/medians['candidate']}
archive=out/'baseline-source.tar.gz'
with tarfile.open(archive,'w:gz')as tar:
 files=[*sorted((baseline/'crates').rglob('*')),*sorted((baseline/'configs').rglob('*')),*sorted((baseline/'scripts').rglob('*.py')),*sorted((baseline/'fixtures/protocol').rglob('*')),*[baseline/n for n in ['Cargo.toml','Cargo.lock','THIRD_PARTY_LICENSES.txt','AGENTS.md']]]
 for path in files:
  if path.is_file():
   info=tar.gettarinfo(str(path),arcname='baseline/'+str(path.relative_to(baseline)));info.uid=info.gid=0;info.uname=info.gname='';info.pax_headers={}
   with path.open('rb')as handle:tar.addfile(info,handle)
assert sha(Path(__file__))==driver_digest,'Benchmark driver changed during timing'
report={'passed':True,'reference_revision':build['reference_revision'],'candidate_build_sha256':sha(ROOT/'results/rust-build.json'),'rounds':a.rounds,'sequential_timing':True,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'changed_rust_sources':changed,'source_assets_sha256':build['source_assets'],'rustc':build['rustc'],'cargo_lock_sha256':sha(ROOT/'Cargo.lock'),'fixture_sha256':fixtures,'baseline_tests_passed':sum(map(int,re.findall(r'test result: ok\. (\d+) passed',tests))),'baseline_tests_log_sha256':sha(out/'baseline-tests.log'),'baseline_source_archive_sha256':sha(archive),'driver_sha256':driver_digest,'separate_cargo_targets':True,'harness_sha256':{n:sha(ROOT/'crates/quai-execution/examples'/(n+'.rs'))for n in harnesses},'binaries':{role:{name:sha(path)for name,path in paths.items()}for role,paths in binary_paths.items()},'rust_sources':{role:{str(path.relative_to(source)):sha(path)for path in sorted((source/'crates').rglob('*.rs'))}for role,source in sources.items()},'metrics':metrics,'full_consensus':False,'canonical_sync':False,'scope':'Controlled prior World root algorithm versus incremental World root algorithm. All other Rust sources/dependencies and each workload are identical. The original clone-root guard may reuse encoding/commitment caches shared through the parent. A separate new first-root workload constructs fresh independent World state before each timer. These are distinct workloads and are not comparable to older historical changed-root results. Setup and verification excluded from timers; roots/gas/checksums must agree. Shared-host sequential medians, no Go timing, network TPS, sync throughput, RSS or physical-disk claim.'}
save(out/'summary.json',report);save(ROOT/'results/latest-incremental-state-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'speedups':{k:round(v['speedup'],3)for k,v in metrics.items()}},indent=2))
