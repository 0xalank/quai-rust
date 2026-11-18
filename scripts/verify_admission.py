#!/usr/bin/env python3
"""Native local-zone component admission against frozen real-Go validation."""
import datetime,json,os,subprocess,tempfile
from pathlib import Path
from lab import ROOT,sha,save
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,'Rebuild first: '+name
assert b['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
corpus=ROOT/'fixtures/protocol/admission.json';evidence=corpus.with_name('admission-source.json');m=json.loads(evidence.read_text())
assert m['reference_revision']==b['reference_revision']and m['vectors_sha256']==sha(corpus)
assert m['profile_sha256']==sha(ROOT/'configs/private-admission-v1.json')
for key,name in [('driver_sha256','scripts/gen_admission_vectors.py'),('oracle_source_sha256','reference/admission/main.go'),('adapter_sha256','reference/admission/core-export.go'),('harness_sha256','reference/admission/harness.go'),('negative_sha256','reference/admission/negative.go')]:assert m[key]==sha(ROOT/name)
out=ROOT/'results'/('admission-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()
with (out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-admission'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=target/'release/examples/check-admission'
with tempfile.TemporaryDirectory(prefix='quai-local-admission-')as directory:r=subprocess.run([str(binary),str(corpus),directory],cwd=ROOT,text=True,capture_output=True)
(out/'check.log').write_text(r.stdout+r.stderr);assert r.returncode==0
result=json.loads(r.stdout);assert result['passed']and result['unique_native_blocks']==24 and result['valid_work_forgeries_rejected']==31
save(out/'checks.json',result)
summary={**result,'reference_revision':b['reference_revision'],'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(corpus),'evidence_sha256':sha(evidence),'checker_sha256':sha(binary),'profile_sha256':sha(ROOT/'configs/private-admission-v1.json'),'public_network':False,'global_header_admission':False,'history_verified_from_network':False}
save(out/'summary.json',summary);save(ROOT/'results/latest-admission.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'checks':result['checks'],'valid_work_forgeries_rejected':31},indent=2))
