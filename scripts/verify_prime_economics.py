#!/usr/bin/env python3
"""Verify frozen native composed parent controller fields; Go is not invoked."""
import datetime,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,sha,save
build=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in build[group].items():assert sha(ROOT/name)==digest,name
corpus=ROOT/'fixtures/protocol/prime-economics.json';source=corpus.with_name('prime-economics-source.json')
m=json.loads(source.read_text());assert m['reference_revision']==build['reference_revision'] and m['vectors_sha256']==sha(corpus)
assert m['driver_sha256']==sha(ROOT/'scripts/gen_prime_economics_vectors.py')
for name,digest in m['oracle_sources'].items():assert sha(ROOT/name)==digest,name
out=ROOT/'results'/('prime-economics-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir()
shutil.copy2(Path(__file__),out/'qualification-driver.py')
with (out/'build.log').open('w') as log:
 subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-prime-economics'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'check-prime-economics';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-prime-economics',binary)
result=subprocess.run([str(binary),str(corpus)],cwd=ROOT,capture_output=True,text=True,check=True)
(out/'native.log').write_text(result.stdout+result.stderr);q=json.loads(result.stdout)
assert q['passed'] and q['go_field_comparisons']==1024 and not q['canonical_authority'] and not q['full_consensus']
assert q['cases']==256 and q['native_checks']==5120
assert q['go_field_mutations_rejected']==768 and q['go_missing_history_rejected']==512
assert q['native_missing_inputs_rejected']==768 and q['native_bindings_rejected']==1280
assert q['max_window_rows_copied']==64 and q['supplied_history']
summary={**q,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':build['reference_revision'],
 'corpus_sha256':sha(corpus),'source_sha256':sha(source),'checker_artifact':str(binary.relative_to(ROOT)),'checker_sha256':sha(binary),
 'qualification_driver_sha256':sha(Path(__file__)),'timing_measured':False,'heap_or_rss_measured':False,
 'scope':m['scope'],'evidence_files':{p.name:sha(p) for p in out.iterdir() if p.is_file()}}
save(out/'summary.json',summary);save(ROOT/'results/latest-prime-economics.json',{'passed':True,'directory':str(out.relative_to(ROOT))})
print(json.dumps(q,indent=2))
