#!/usr/bin/env python3
"""Execute frozen Go component transitions through local keyed state versions."""
import datetime,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,save,sha
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,'Rebuild first: '+name
assert b['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
corpora={}
for name,driver in [('settlement','gen_settlement_vectors.py'),('qi-settlement','gen_qi_settlement_vectors.py'),('incoming','gen_incoming_vectors.py')]:
 corpus=ROOT/'fixtures/protocol'/(name+'.json');evidence=corpus.with_name(name+'-source.json');m=json.loads(evidence.read_text());assert m['reference_revision']==b['reference_revision']and m['vectors_sha256']==sha(corpus)and m['driver_sha256']==sha(ROOT/'scripts'/driver)
 if 'oracle_sources'in m:
  for path,digest in m['oracle_sources'].items():assert sha(ROOT/path)==digest,path
 else:assert m['oracle_source_sha256']==sha(ROOT/'reference'/name/'main.go')
 corpora[name]={'corpus_sha256':sha(corpus),'evidence_sha256':sha(evidence)}
out=ROOT/'results'/('persistent-execution-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
with(out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-execution','--example','check-persistent-execution'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'check-persistent-execution';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-persistent-execution',binary)
with tempfile.TemporaryDirectory(prefix='quai-persisted-components-')as directory:
 r=subprocess.run([str(binary),directory],cwd=ROOT,text=True,capture_output=True,timeout=180)
(out/'check.log').write_text(r.stdout+r.stderr);assert r.returncode==0,r.stderr[-1500:];q=json.loads(r.stdout)
assert q['transition_inputs_use_lazy_etx_and_lockups']and q['transition_inputs_use_lazy_accounts_and_slots']and q['transition_inputs_use_lazy_qi_state']and q['passed']and q['cases']==1371 and q['go_transition_comparisons']==18312 and q['state_roundtrips']==2706 and q['state_versions']==2707 and q['rejected_incoming_transitions']==36
save(out/'summary.json',{**q,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':b['reference_revision'],'corpora':corpora,'checker_sha256':sha(binary),'checker_artifact':str(binary.relative_to(ROOT)),'qualification_driver_sha256':sha(Path(__file__)),'public_network':False})
save(ROOT/'results/latest-persistent-execution.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'cases':q['cases'],'go_comparisons':q['go_transition_comparisons'],'roundtrips':q['state_roundtrips']},indent=2))
