#!/usr/bin/env python3
"""Frozen synthetic account/storage transition, explicit materialized/sparse modes."""
import datetime,json,os,platform,shutil,statistics,subprocess,tarfile
from pathlib import Path
from lab import ROOT,save,sha
DRIVER_DIGEST=sha(Path(__file__))
CONTRACT_DIGEST=sha(ROOT/'configs/benchmark-sparse-world-v1.json')
contract=json.loads((ROOT/'configs/benchmark-sparse-world-v1.json').read_text());assert contract['parent_accounts']==10000 and contract['initial_slots_per_account']==2 and contract['touched_accounts']==[1,2,3] and contract['timing_pairs']==5 and contract['separate_allocation_pairs']==3
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,name
no=json.loads((ROOT/'results/rust-no-go-build.json').read_text());assert no['passed']and no['source_build_sha256']==sha(ROOT/'results/rust-build.json')and no['lazy_world_state_tests_executed']
for name,field in [('persistent-execution','transition_inputs_use_lazy_accounts_and_slots'),('canonical','lazy_accounts'),('private-processes','lazy_accounts')]:
 q=json.loads((ROOT/'results'/('latest-'+name+'.json')).read_text());q=json.loads((ROOT/q['directory']/'summary.json').read_text());assert q['passed']and q['build_sha256']==sha(ROOT/'results/rust-build.json')and q[field]
out=ROOT/'results'/('sparse-world-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-execution','--example','bench-sparse-world'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'bench-sparse-world';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/bench-sparse-world',binary);shutil.copy2(Path(__file__),out/'benchmark-driver.py')
samples=[];stable_reference=None
for mode,rounds in [('timing',5),('allocations',3)]:
 for round in range(rounds):
  for backend in (['materialized','sparse']if round%2==0 else['sparse','materialized']):
   load=os.getloadavg();q=json.loads(subprocess.check_output([str(binary),backend,'10000']+(['--allocations']if mode=='allocations'else[]),cwd=ROOT,text=True,timeout=180))
   assert q['backend']==backend and q['mode']==mode and q['parent_accounts']==10000 and q['parent_storage_slots']==20000 and q['touched_accounts']==3 and q['storage_writes']==2 and not q['native_block_admission']
   assert q['loaded_accounts_during_transition']<=3 if backend=='sparse'else q['loaded_accounts_during_transition']==10000
   stable={k:v for k,v in q.items()if k not in ['backend','mode','loaded_accounts_during_transition','ns_per_transition','gross_allocated_bytes','allocation_requests']}
   if stable_reference is None:stable_reference=stable
   else:assert stable==stable_reference
   samples.append({'round':round,'backend':backend,'mode':mode,'load_average':load,'result':q});save(out/'raw.json',samples)
metrics={}
for metric,mode in [('ns_per_transition','timing'),('gross_allocated_bytes','allocations'),('allocation_requests','allocations')]:
 values={backend:[s['result'][metric]for s in samples if s['mode']==mode and s['backend']==backend]for backend in ['materialized','sparse']};medians={k:statistics.median(v)for k,v in values.items()};metrics[metric]={'samples':values,'medians':medians,'ratio_materialized_over_sparse':medians['materialized']/medians['sparse'],'sparse_change_percent':100*(medians['sparse']/medians['materialized']-1)}
assert sha(Path(__file__))==DRIVER_DIGEST and sha(ROOT/'configs/benchmark-sparse-world-v1.json')==CONTRACT_DIGEST
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
summary={'passed':True,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':b['reference_revision'],'same_source_and_binary_for_both_modes':True,'configuration_comparison':True,'backend_arguments':['materialized','sparse'],'fixed_parent_accounts':10000,'fixed_parent_storage_slots':20000,'fixed_touched_accounts':3,'fixed_storage_writes':2,'source_archive_sha256':sha(archive),'binary_sha256':sha(binary),'driver_sha256':DRIVER_DIGEST,'harness_sha256':sha(ROOT/'crates/quai-execution/examples/bench-sparse-world.rs'),'rustc':b['rustc'],'rust_sources':b['rust_sources'],'source_assets':b['source_assets'],'cargo_lock_sha256':sha(ROOT/'Cargo.lock'),'timing_pairs':5,'separate_allocation_pairs':3,'sequential_timing':True,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'metrics':metrics,'full_consensus':False,'canonical_public_sync':False,'native_block_admission':False,'account_counter_scope':'Materialized mode reports its loaded membership size; sparse mode counts decoded requested accounts and explicit diagnostic exports. The captured counter precedes final diagnostic export.','contract_sha256':sha(ROOT/'configs/benchmark-sparse-world-v1.json'),'scope':'One current Rust binary in two explicit account/contract-storage restoration configurations, with the same codec-2 database and a synthetic 10,000-account / 20,000-slot parent. A journal changes two contract slots and transfers a balance across three touched accounts; block storage-size finalization and durable persistence produce an identical child. Both modes check exact account/storage membership, consensus roots, local root-set digest and version identity. Includes fresh snapshot, restore, clone, journal updates/finalization and transactional persistence. Database/cache initialization, parent generation/commit, expected-state construction, final full diagnostic export/checks and cleanup are excluded. Allocations run separately from timing and are gross requested bytes, not retained heap or RSS. This is synthetic storage/journal work, not a signed transaction, native block, Go, network or cold-disk measurement.'}
save(out/'summary.json',summary);save(ROOT/'results/latest-sparse-world-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'metrics':metrics},indent=2))
