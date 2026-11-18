#!/usr/bin/env python3
"""Frozen synthetic Qi storage transition, explicit materialized/sparse modes."""
import datetime,json,os,platform,shutil,statistics,subprocess,tarfile
from pathlib import Path
from lab import ROOT,save,sha
DRIVER_DIGEST=sha(Path(__file__))
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,name
no=json.loads((ROOT/'results/rust-no-go-build.json').read_text());assert no['passed']and no['source_build_sha256']==sha(ROOT/'results/rust-build.json')and no['lazy_qi_state_tests_executed']
for name,field in [('persistent-execution','transition_inputs_use_lazy_qi_state'),('canonical','lazy_qi'),('private-processes','lazy_qi')]:
 q=json.loads((ROOT/'results'/('latest-'+name+'.json')).read_text());q=json.loads((ROOT/q['directory']/'summary.json').read_text());assert q['passed']and q['build_sha256']==sha(ROOT/'results/rust-build.json')and q[field]
out=ROOT/'results'/('sparse-qi-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-execution','--example','bench-sparse-qi'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'bench-sparse-qi';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/bench-sparse-qi',binary);shutil.copy2(Path(__file__),out/'benchmark-driver.py')
samples=[];stable_reference=None
for mode,rounds in [('timing',5),('allocations',3)]:
 for round in range(rounds):
  for backend in (['materialized','sparse']if round%2==0 else['sparse','materialized']):
   load=os.getloadavg();q=json.loads(subprocess.check_output([str(binary),backend,'50000']+(['--allocations']if mode=='allocations'else[]),cwd=ROOT,text=True,timeout=180))
   assert q['backend']==backend and q['mode']==mode and q['parent_raw_entries']==50000 and q['touched_membership_keys']==8 and not q['native_block_admission']
   assert q['point_records_decoded']<=8 if backend=='sparse'else q['point_records_decoded']==0
   stable={k:v for k,v in q.items()if k not in ['backend','mode','point_records_decoded','ns_per_transition','gross_allocated_bytes','allocation_requests']}
   if stable_reference is None:stable_reference=stable
   else:assert stable==stable_reference
   samples.append({'round':round,'backend':backend,'mode':mode,'load_average':load,'result':q});save(out/'raw.json',samples)
metrics={}
for metric,mode in [('ns_per_transition','timing'),('gross_allocated_bytes','allocations'),('allocation_requests','allocations')]:
 values={backend:[s['result'][metric]for s in samples if s['mode']==mode and s['backend']==backend]for backend in ['materialized','sparse']};medians={k:statistics.median(v)for k,v in values.items()};metrics[metric]={'samples':values,'medians':medians,'ratio_materialized_over_sparse':medians['materialized']/medians['sparse'],'sparse_change_percent':100*(medians['sparse']/medians['materialized']-1)}
assert sha(Path(__file__))==DRIVER_DIGEST
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,name
archive=out/'source-and-fixtures.tar.gz'
with tarfile.open(archive,'w:gz')as tar:
 files=[*sorted((ROOT/'crates').rglob('*')),*sorted((ROOT/'configs').rglob('*')),*sorted((ROOT/'fixtures/protocol').rglob('*')),*sorted((ROOT/'scripts').glob('*.py')),*[ROOT/n for n in ['Cargo.toml','Cargo.lock','THIRD_PARTY_LICENSES.txt','AGENTS.md']]]
 for path in files:
  if path.is_file():
   info=tar.gettarinfo(str(path),arcname='source/'+str(path.relative_to(ROOT)));info.uid=info.gid=0;info.uname=info.gname='';info.pax_headers={}
   with path.open('rb')as file:tar.addfile(info,file)
shutil.copy2(ROOT/'results/rust-build.json',out/'rust-build.json')
summary={'passed':True,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':b['reference_revision'],'same_source_and_binary_for_both_modes':True,'configuration_comparison':True,'backend_arguments':['materialized','sparse'],'fixed_parent_raw_entries':50000,'fixed_removed_entries':4,'fixed_created_entries':4,'source_archive_sha256':sha(archive),'binary_sha256':sha(binary),'driver_sha256':DRIVER_DIGEST,'harness_sha256':sha(ROOT/'crates/quai-execution/examples/bench-sparse-qi.rs'),'rustc':b['rustc'],'rust_sources':b['rust_sources'],'source_assets':b['source_assets'],'cargo_lock_sha256':sha(ROOT/'Cargo.lock'),'timing_pairs':5,'separate_allocation_pairs':3,'sequential_timing':True,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'metrics':metrics,'full_consensus':False,'canonical_public_sync':False,'native_block_admission':False,'point_counter_scope':'Only sparse provider point records are counted; the materialized scan is not reflected in its zero point counter.','scope':'One current Rust binary with two explicit Qi restoration/storage configurations. A fixed synthetic 50,000-UTXO local parent undergoes four removals, four insertions and a durable child commit. Both modes check exact final raw membership, MuHash/count, local root-set digest and version identity. Includes fresh snapshot creation, parent restore, cloning, membership updates and transactional persistence. Initial database/cache allocation, parent generation/commit, expected-result construction and final full export/checks are excluded. Timing and allocation runs are separate. This is synthetic storage work, not a signed-Qi, native block, Go, canonical catalog, network, cold-disk, retained-memory or RSS benchmark. Correctness of actual signed Qi/incoming transitions is qualified separately against the frozen Go corpora.'}
save(out/'summary.json',summary);save(ROOT/'results/latest-sparse-qi-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'metrics':metrics},indent=2))
