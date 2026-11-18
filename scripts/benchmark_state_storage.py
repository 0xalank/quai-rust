#!/usr/bin/env python3
"""Same source/binary and frozen hierarchy workload, two explicit storage modes."""
import datetime,json,os,platform,shutil,statistics,subprocess,tarfile
from pathlib import Path
from lab import ROOT,save,sha
driver_digest=sha(Path(__file__))
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,name
for name in ['persistent-execution','canonical','private-processes']:
 q=json.loads((ROOT/'results'/('latest-'+name+'.json')).read_text());q=json.loads((ROOT/q['directory']/'summary.json').read_text());assert q['passed']and q['build_sha256']==sha(ROOT/'results/rust-build.json')
corpus=ROOT/'fixtures/protocol/confirmation.json';manifest=json.loads(corpus.with_name('confirmation-source.json').read_text());assert manifest['vectors_sha256']==sha(corpus)and manifest['reference_revision']==b['reference_revision']
out=ROOT/'results'/('state-storage-performance-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','bench-state-storage'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'bench-state-storage';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/bench-state-storage',binary)
shutil.copy2(Path(__file__),out/'benchmark-driver.py');samples=[]
for mode,rounds in [('timing',5),('allocations',3)]:
 for round in range(rounds):
  for backend in (['images','database']if round%2==0 else['database','images']):
   args=[str(binary),str(corpus),backend]+(['--allocations']if mode=='allocations'else[])
   load=os.getloadavg();q=json.loads(subprocess.check_output(args,cwd=ROOT,text=True));assert q['backend']==backend and q['mode']==mode and q['blocks']==16 and q['executed_inbound_etxs']==8;samples.append({'round':round,'mode':mode,'backend':backend,'load_average':load,'result':q});save(out/'raw.json',samples)
reference=None
for s in samples:
 stable={k:v for k,v in s['result'].items()if k not in ['backend','mode','ns_per_block','gross_allocated_bytes_per_block','allocation_requests_per_block']}
 if reference is None:reference=stable
 else:assert stable==reference
metrics={}
for metric,mode in [('ns_per_block','timing'),('gross_allocated_bytes_per_block','allocations'),('allocation_requests_per_block','allocations')]:
 values={backend:[s['result'][metric]for s in samples if s['mode']==mode and s['backend']==backend]for backend in ['images','database']};medians={k:statistics.median(v)for k,v in values.items()};metrics[metric]={'samples':values,'medians':medians,'ratio_images_over_database':medians['images']/medians['database'],'database_change_percent':100*(medians['database']/medians['images']-1)}
assert sha(Path(__file__))==driver_digest
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
save(out/'summary.json',{'passed':True,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':b['reference_revision'],'same_source_and_binary_for_both_modes':True,'source_archive_sha256':sha(archive),'rustc':b['rustc'],'backend_arguments':['images','database'],'configuration_comparison':True,'sequential_timing':True,'allocation_runs_separate':True,'shared_host':True,'host':{'system':platform.system(),'machine':platform.machine(),'cpu_count':os.cpu_count()},'corpus_sha256':sha(corpus),'corpus_evidence_sha256':sha(corpus.with_name('confirmation-source.json')),'harness_sha256':sha(ROOT/'crates/quai-consensus/examples/bench-state-storage.rs'),'binary_sha256':sha(binary),'driver_sha256':driver_digest,'metrics':metrics,'rust_sources':b['rust_sources'],'source_assets':b['source_assets'],'cargo_lock_sha256':sha(ROOT/'Cargo.lock'),'full_consensus':False,'canonical_public_sync':False,'scope':'Same current Rust build with explicit complete-image versus keyed-database private branch storage modes. Fixed 16-block early hierarchy/coinbase workload, identical warm native proof verifier and exact final block/EVM/ETX checks. Includes execution, parent restoration, state staging and durable execution-head selection; parsing, genesis setup, initial database/cache allocation, proof warming and final checking excluded. Gross requested allocations are not retained memory or RSS. No Go timing, full-node, native canonical catalog, network-sync or physical-disk claim.'})
save(ROOT/'results/latest-state-storage-performance.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'metrics':metrics},indent=2))
