#!/usr/bin/env python3
"""Qualify native activated etx-family hierarchy components; supplied history grants no authority."""
import datetime,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,save,sha

b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,'Rebuild first: '+name
corpus=ROOT/'fixtures/protocol/etx-family.json';source=corpus.with_name('etx-family-source.json');m=json.loads(source.read_text());assert m['reference_revision']==b['reference_revision']and m['vectors_sha256']==sha(corpus)and m['driver_sha256']==sha(ROOT/'scripts/gen_etx_family_vectors.py')and m['profile_sha256']==sha(ROOT/'configs/private-hierarchy-v1.json')
for name,digest in m['oracle_sources'].items():assert sha(ROOT/name)==digest,name
out=ROOT/'results'/('etx-family-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir();shutil.copy2(Path(__file__),out/'qualification-driver.py')
with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-etx-family'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'check-etx-family';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-etx-family',binary)
with tempfile.TemporaryDirectory(prefix='quai-etx-family-components-')as directory:r=subprocess.run([str(binary),str(corpus),directory],cwd=ROOT,text=True,capture_output=True,timeout=180)
(out/'check.log').write_text(r.stdout+r.stderr);assert r.returncode==0,r.stderr[-1800:];q=json.loads(r.stdout)
assert q['external_kinds_executed']==[0,1,2,3,4,5,6]
assert q['passed']and q['checks']==4068 and q['branch_runs']==32 and q['distinct_conditional_blocks']==24 and q['valid_work_rejections']==246 and q['rejected_attempts']==492 and q['backends']==2 and q['state_roundtrips']==66 and q['writer_reopens']==16 and q['context_checks']==100 and q['execution_inbounds']==44 and q['orders']==[12,12,40]and q['shared_prefix_versions_reused']==16 and q['fork_isolation_checks']==2 and q['persisted_hierarchy_outputs']and q['lazy_state_and_history_checked']and not q['checkpoint_ancestry_verified']and not q['canonical_publication']and not q['global_full_consensus']and not q['canonical_public_sync']
for name in [corpus,source]:shutil.copy2(name,out/name.name)
summary={**q,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':b['reference_revision'],'corpus_sha256':sha(corpus),'evidence_sha256':sha(source),'checker_artifact':str(binary.relative_to(ROOT)),'checker_sha256':sha(binary),'qualification_driver_sha256':sha(Path(__file__)),'public_network':False,'scope':m['scope'],'component_coverage_only':True,'private_canonical_runtime_profile_widened':False,'evidence_files':{p.name:sha(p)for p in out.iterdir()if p.is_file()}};save(out/'summary.json',summary);save(ROOT/'results/latest-etx-family.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({k:summary[k]for k in ['passed','checks','branch_runs','distinct_conditional_blocks','valid_work_rejections','state_roundtrips','writer_reopens','canonical_publication']},indent=2))
