#!/usr/bin/env python3
"""Compare frozen parent-restore/commit workload across two qualified Rust sources."""
import argparse,datetime,json,os,platform,shutil,statistics,subprocess,tarfile,tempfile
from pathlib import Path
from lab import ROOT,save,sha

def main():
 p=argparse.ArgumentParser();p.add_argument('--baseline',type=Path,required=True);args=p.parse_args();before=args.baseline.resolve();driver=Path(__file__);driver_digest=sha(driver)
 assert before!=ROOT
 builds={role:json.loads((source/'results/rust-build.json').read_text())for role,source in [('before',before),('after',ROOT)]}
 sources={'before':before,'after':ROOT};harness='crates/quai-consensus/examples/bench-state-storage.rs';corpus='fixtures/protocol/confirmation.json'
 for role,source in sources.items():
  b=builds[role];assert b['reference_revision']=='33db82f2891b267ba898ca38bd2dd8bf75cbb072'and not b['go_runtime_dependency']
  for group in ['rust_sources','source_assets']:
   for name,digest in b[group].items():assert sha(source/name)==digest,(role,name)
 assert sha(before/harness)==sha(ROOT/harness),'Freeze the common benchmark harness'
 for name in [corpus,corpus.replace('.json','-source.json'),'Cargo.lock','configs/private-hierarchy-v1.json']:
  assert sha(before/name)==sha(ROOT/name),name
 baseline_report=next((before/'results'/name for name in ['activation-final-verification.json','lazy-aux-final-verification.json','lazy-world-final-verification.json','state-lease-final-verification.json','lazy-qi-final-verification.json','catalog-final-verification.json','canonical-db-final-verification.json']if(before/'results'/name).exists()))
 previous=json.loads(baseline_report.read_text());assert previous['passed']and previous['source_build_sha256']==sha(before/'results/rust-build.json')
 for name in ['persistent-execution','canonical','private-processes']:
  q=json.loads((ROOT/'results'/('latest-'+name+'.json')).read_text());q=json.loads((ROOT/q['directory']/'summary.json').read_text());assert q['passed']and q['build_sha256']==sha(ROOT/'results/rust-build.json')
 out=ROOT/'results'/('indexed-history-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True);shutil.copy2(driver,out/'benchmark-driver.py');artifacts={}
 for role,source in sources.items():
  directory=out/role;directory.mkdir();shutil.copy2(source/'results/rust-build.json',directory/'rust-build.json')
  # Independent target directories prevent Cargo's fixed example executable path
  # from aliasing baseline and candidate outputs from different checkouts.
  with tempfile.TemporaryDirectory(prefix='quai-indexed-'+role+'-target-')as target:
   env={**os.environ,'CARGO_TARGET_DIR':target,'CARGO_NET_OFFLINE':'true'}
   with(directory/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','bench-state-storage'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
   binary=directory/'bench-state-storage';shutil.copy2(Path(target)/'release/examples/bench-state-storage',binary)
  archive=directory/'source-and-fixtures.tar.gz'
  with tarfile.open(archive,'w:gz')as tar:
   files=[*sorted((source/'crates').rglob('*')),*sorted((source/'configs').rglob('*')),*sorted((source/'fixtures/protocol').rglob('*')),*sorted((source/'scripts').glob('*.py')),*[source/n for n in ['Cargo.toml','Cargo.lock','THIRD_PARTY_LICENSES.txt','AGENTS.md']]]
   for path in files:
    if path.is_file():
     info=tar.gettarinfo(str(path),arcname='source/'+str(path.relative_to(source)));info.uid=info.gid=0;info.uname=info.gname='';info.pax_headers={}
     with path.open('rb')as file:tar.addfile(info,file)
  artifacts[role]={'binary_sha256':sha(binary),'source_archive_sha256':sha(archive),'build_sha256':sha(source/'results/rust-build.json'),'rust_sources':builds[role]['rust_sources'],'source_assets':builds[role]['source_assets']}
 assert artifacts['before']['binary_sha256']!=artifacts['after']['binary_sha256'],'Distinct source builds must yield distinct benchmark binaries'
 samples=[];stable_reference=None
 for mode,rounds in [('timing',7),('allocations',3)]:
  for round in range(rounds):
   for role in (['before','after']if round%2==0 else['after','before']):
    binary=out/role/'bench-state-storage';assert sha(binary)==artifacts[role]['binary_sha256'];load=os.getloadavg()
    q=json.loads(subprocess.check_output([str(binary),str(ROOT/corpus),'database']+(['--allocations']if mode=='allocations'else[]),cwd=ROOT,text=True))
    assert q['backend']=='database'and q['mode']==mode and q['blocks']==16 and q['executed_inbound_etxs']==8
    stable={k:v for k,v in q.items()if k not in ['mode','ns_per_block','gross_allocated_bytes_per_block','allocation_requests_per_block']}
    if stable_reference is None:stable_reference=stable
    else:assert stable==stable_reference
    samples.append({'round':round,'role':role,'mode':mode,'load_average':load,'result':q});save(out/'raw.json',samples)
 metrics={}
 for metric,mode in [('ns_per_block','timing'),('gross_allocated_bytes_per_block','allocations'),('allocation_requests_per_block','allocations')]:
  values={role:[s['result'][metric]for s in samples if s['mode']==mode and s['role']==role]for role in sources};medians={k:statistics.median(v)for k,v in values.items()};metrics[metric]={'samples':values,'medians':medians,'ratio_before_over_after':medians['before']/medians['after'],'change_percent':100*(medians['after']/medians['before']-1)}
 assert sha(driver)==driver_digest
 for role,source in sources.items():
  assert sha(source/'results/rust-build.json')==artifacts[role]['build_sha256']
  for group in ['rust_sources','source_assets']:
   for name,digest in builds[role][group].items():assert sha(source/name)==digest,(role,name)
 save(out/'summary.json',{'passed':True,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':builds['after']['reference_revision'],'source_comparison':True,'isolated_cargo_targets':True,'same_database_configuration':True,'sequential_timing':True,'timing_pairs':7,'separate_allocation_pairs':3,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'artifacts':artifacts,'harness_sha256':sha(ROOT/harness),'corpus_sha256':sha(ROOT/corpus),'evidence_sha256':sha(ROOT/corpus.replace('.json','-source.json')),'driver_sha256':driver_digest,'metrics':metrics,'full_consensus':False,'canonical_public_sync':False,'scope':'Before/after Rust source comparison with the identical 16-block hierarchy/coinbase workload and keyed database configuration. Includes native execution with warm proof work, fresh parent restoration, branch staging and durable execution-head selection. Parsing, initial database/cache and genesis setup, proof warming and final checking excluded. Canonical admission catalog, public sync, long-history scaling, Go performance, physical disk, retained memory and RSS are not measured. Gross requested allocation bytes include reallocations; startup cache allocation is excluded.'})
 save(ROOT/'results/latest-indexed-history-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'metrics':metrics},indent=2))
if __name__=='__main__':main()
