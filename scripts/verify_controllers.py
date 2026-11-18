#!/usr/bin/env python3
"""Verify frozen native economic arithmetic; Go is not invoked."""
import datetime,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,sha,save
build=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in build[group].items():assert sha(ROOT/name)==digest,name
corpus=ROOT/'fixtures/protocol/controllers.json';source=corpus.with_name('controllers-source.json')
m=json.loads(source.read_text());assert m['reference_revision']==build['reference_revision'] and m['vectors_sha256']==sha(corpus)
assert m['driver_sha256']==sha(ROOT/'scripts/gen_controller_vectors.py')
for name,digest in m['oracle_sources'].items():assert sha(ROOT/name)==digest,name
out=ROOT/'results'/('controllers-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir()
shutil.copy2(Path(__file__),out/'qualification-driver.py')
with (out/'build.log').open('w') as log:
 subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-controllers'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'check-controllers';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-controllers',binary)
result=subprocess.run([str(binary),str(corpus)],cwd=ROOT,capture_output=True,text=True,check=True)
(out/'native.log').write_text(result.stdout+result.stderr);q=json.loads(result.stdout)
assert q['passed'] and q['go_comparisons']==3200 and not q['canonical_authority'] and not q['full_consensus']
assert all(q[name]==640 for name in ['flow_cases','difficulty_cases','discount_cases','update_cases','exchange_rate_cases'])
summary={**q,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':build['reference_revision'],
 'corpus_sha256':sha(corpus),'source_sha256':sha(source),'checker_artifact':str(binary.relative_to(ROOT)),'checker_sha256':sha(binary),
 'qualification_driver_sha256':sha(Path(__file__)),'timing_measured':False,'heap_or_rss_measured':False,
 'scope':m['scope'],'evidence_files':{p.name:sha(p) for p in out.iterdir() if p.is_file()}}
save(out/'summary.json',summary);save(ROOT/'results/latest-controllers.json',{'passed':True,'directory':str(out.relative_to(ROOT))})
print(json.dumps(q,indent=2))
