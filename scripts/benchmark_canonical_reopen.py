#!/usr/bin/env python3
"""Fixed native canonical history, fresh-writer recovery, two frozen Rust sources."""
import argparse,datetime,json,os,platform,shutil,statistics,subprocess,tarfile,tempfile
from pathlib import Path
from lab import ROOT,save,sha

def main():
 p=argparse.ArgumentParser();p.add_argument('--baseline',type=Path,required=True);args=p.parse_args();before=args.baseline.resolve();driver=Path(__file__);driver_digest=sha(driver)
 sources={'before':before,'after':ROOT};builds={role:json.loads((source/'results/rust-build.json').read_text())for role,source in sources.items()}
 harness='crates/quai-consensus/examples/bench-canonical-reopen.rs';corpus='fixtures/protocol/canonical.json'
 for role,source in sources.items():
  b=builds[role];assert b['reference_revision']=='33db82f2891b267ba898ca38bd2dd8bf75cbb072'and not b['go_runtime_dependency']
  for group in ['rust_sources','source_assets']:
   for name,digest in b[group].items():assert sha(source/name)==digest,(role,name)
 for name in [corpus,corpus.replace('.json','-source.json'),'Cargo.lock','configs/private-hierarchy-v1.json']:assert sha(before/name)==sha(ROOT/name),name
 baseline_report=next((before/'results'/name for name in ['activation-final-verification.json','lazy-aux-final-verification.json','lazy-world-final-verification.json','state-lease-final-verification.json','lazy-qi-final-verification.json','catalog-final-verification.json','canonical-db-final-verification.json']if(before/'results'/name).exists()))
 previous=json.loads(baseline_report.read_text());assert previous['passed']and previous['source_build_sha256']==sha(before/'results/rust-build.json')
 baseline_needs_harness=not(before/harness).exists()
 if not baseline_needs_harness:assert sha(before/harness)==sha(ROOT/harness)
 for name in ['canonical','private-processes']:
  q=json.loads((ROOT/'results'/('latest-'+name+'.json')).read_text());q=json.loads((ROOT/q['directory']/'summary.json').read_text());assert q['passed']and q['build_sha256']==sha(ROOT/'results/rust-build.json')and q['indexed_catalog']
 out=ROOT/'results'/('canonical-reopen-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True);shutil.copy2(driver,out/'benchmark-driver.py');artifacts={}
 for role,original in sources.items():
  directory=out/role;directory.mkdir();shutil.copy2(original/'results/rust-build.json',directory/'rust-build.json')
  with tempfile.TemporaryDirectory(prefix='quai-reopen-'+role+'-')as temporary:
   temporary=Path(temporary);source=original
   if role=='before'and baseline_needs_harness:
    source=temporary/'source';source.mkdir()
    for name in ['crates','configs','fixtures','scripts']:shutil.copytree(original/name,source/name,ignore=shutil.ignore_patterns('__pycache__'))
    for name in ['Cargo.toml','Cargo.lock','THIRD_PARTY_LICENSES.txt','AGENTS.md']:shutil.copy2(original/name,source/name)
    assert not(source/harness).exists();shutil.copy2(ROOT/harness,source/harness)
   assert sha(source/harness)==sha(ROOT/harness)
   env={**os.environ,'CARGO_TARGET_DIR':str(temporary/'target'),'CARGO_NET_OFFLINE':'true'}
   with(directory/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','bench-canonical-reopen'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
   binary=directory/'bench-canonical-reopen';shutil.copy2(temporary/'target/release/examples/bench-canonical-reopen',binary)
   archive=directory/'source-and-fixtures.tar.gz'
   with tarfile.open(archive,'w:gz')as tar:
    files=[*sorted((source/'crates').rglob('*')),*sorted((source/'configs').rglob('*')),*sorted((source/'fixtures/protocol').rglob('*')),*sorted((source/'scripts').glob('*.py')),*[source/n for n in ['Cargo.toml','Cargo.lock','THIRD_PARTY_LICENSES.txt','AGENTS.md']]]
    for path in files:
     if path.is_file():
      info=tar.gettarinfo(str(path),arcname='source/'+str(path.relative_to(source)));info.uid=info.gid=0;info.uname=info.gname='';info.pax_headers={}
      with path.open('rb')as file:tar.addfile(info,file)
  artifacts[role]={'binary_sha256':sha(binary),'source_archive_sha256':sha(archive),'qualified_build_sha256':sha(original/'results/rust-build.json'),'qualified_rust_sources':builds[role]['rust_sources'],'source_assets':builds[role]['source_assets'],'common_harness_added_to_baseline_copy':role=='before'and baseline_needs_harness,'common_harness_sha256':sha(ROOT/harness)}
 assert artifacts['before']['binary_sha256']!=artifacts['after']['binary_sha256']
 samples=[];stable_reference=None
 for mode,rounds in [('timing',5),('allocations',3)]:
  for round in range(rounds):
   for role in (['before','after']if round%2==0 else['after','before']):
    binary=out/role/'bench-canonical-reopen';assert sha(binary)==artifacts[role]['binary_sha256'];load=os.getloadavg()
    q=json.loads(subprocess.check_output([str(binary),str(ROOT/corpus)]+(['--allocations']if mode=='allocations'else[]),cwd=ROOT,text=True))
    assert q['mode']==mode and q['events']==25 and q['unique_native_blocks']==24 and not q['global_full_consensus']and not q['canonical_public_sync']
    stable={k:v for k,v in q.items()if k not in ['mode','ns_per_reopen','gross_allocated_bytes_per_reopen','allocation_requests_per_reopen']}
    if stable_reference is None:stable_reference=stable
    else:assert stable==stable_reference
    samples.append({'round':round,'role':role,'mode':mode,'load_average':load,'result':q});save(out/'raw.json',samples)
 metrics={}
 for metric,mode in [('ns_per_reopen','timing'),('gross_allocated_bytes_per_reopen','allocations'),('allocation_requests_per_reopen','allocations')]:
  values={role:[s['result'][metric]for s in samples if s['mode']==mode and s['role']==role]for role in sources};medians={k:statistics.median(v)for k,v in values.items()};metrics[metric]={'samples':values,'medians':medians,'ratio_before_over_after':medians['before']/medians['after'],'change_percent':100*(medians['after']/medians['before']-1)}
 assert sha(driver)==driver_digest
 for role,source in sources.items():
  assert sha(source/'results/rust-build.json')==artifacts[role]['qualified_build_sha256']
  for group in ['rust_sources','source_assets']:
   for name,digest in builds[role][group].items():assert sha(source/name)==digest,(role,name)
 save(out/'summary.json',{'passed':True,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':builds['after']['reference_revision'],'source_comparison':True,'new_frozen_recovery_workload':baseline_needs_harness,'identical_common_harness':True,'baseline_copy_adds_only_common_harness':baseline_needs_harness,'isolated_cargo_targets':True,'sequential_timing':True,'timing_pairs':5,'separate_allocation_pairs':3,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'artifacts':artifacts,'harness_sha256':sha(ROOT/harness),'corpus_sha256':sha(ROOT/corpus),'evidence_sha256':sha(ROOT/corpus.replace('.json','-source.json')),'driver_sha256':driver_digest,'metrics':metrics,'full_consensus':False,'canonical_public_sync':False,'scope':'Fixed native private canonical 25-event/24-block history, exact Go selection/head assertions during setup and exact final head/height/score/state-root checks after reopening. Measures a fresh writer open on the warm filesystem after all views/writers are dropped. Includes database/cache initialization and recovery; corpus parsing, initial native imports and post-timer checks excluded. The process retains immutable profile initialization from setup; reopened verifier starts fresh. Both qualified sources use indexed local admission receipts/frontiers in this comparison. No Go, public sync, cold-disk, full-node throughput or RSS measurement. Gross requested allocations include reallocations and startup; the separate catalog has a configured 16 MiB cache in addition to the 64 MiB execution database cache.'})
 save(ROOT/'results/latest-canonical-reopen-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'metrics':metrics},indent=2))
if __name__=='__main__':main()
