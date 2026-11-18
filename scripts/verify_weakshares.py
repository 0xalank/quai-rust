#!/usr/bin/env python3
"""Native weak-share arithmetic, PoEM and branch execution against pinned Go."""
import datetime,json,os,subprocess,tempfile,re
from pathlib import Path
from lab import ROOT,sha,save
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,'Rebuild first: '+name
assert b['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
corpus=ROOT/'fixtures/protocol/weakshares.json';evidence=corpus.with_name('weakshares-source.json');m=json.loads(evidence.read_text())
assert m['reference_revision']==b['reference_revision']and m['vectors_sha256']==sha(corpus)
assert m['profile_sha256']==sha(ROOT/'configs/private-hierarchy-v1.json')
for key,name in [('driver_sha256','scripts/gen_weakshares_vectors.py'),('oracle_source_sha256','reference/weakshares/main.go'),('adapter_sha256','reference/weakshares/core-export.go'),('harness_sha256','reference/weakshares/harness.go'),('arithmetic_adapter_sha256','reference/weakshares/weights.go')]:assert m[key]==sha(ROOT/name)
out=ROOT/'results'/('weakshares-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()
with (out/'build.log').open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-weakshares'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=target/'release/examples/check-weakshares'
with tempfile.TemporaryDirectory(prefix='quai-local-weakshares-')as directory:r=subprocess.run([str(binary),str(corpus),directory],cwd=ROOT,text=True,capture_output=True)
(out/'check.log').write_text(r.stdout+r.stderr);assert r.returncode==0
result=json.loads(r.stdout);assert result['passed']and result['unique_native_blocks']==24 and result['native_context_components']==38 and result['executed_inbound_etxs']==30 and result['checks']==760 and result['invalid_confirmation_inputs_rejected']==11 and result['included_shares']==23 and result['weak_shares']==10 and result['native_candidate_scores']==15 and result['unsupported_go_valid_weak_share_cases']==0 and result['accepted_go_valid_weak_share_cases']==1
save(out/'checks.json',result)
math=subprocess.run(['cargo','test','--release','--locked','--offline','-p','quai-consensus','--test','weak_float','--','--nocapture'],cwd=ROOT,text=True,capture_output=True);(out/'math.log').write_text(math.stdout+math.stderr);assert math.returncode==0
count=re.search(r'WEAK_FLOAT_CHECKS (\d+)',math.stdout);assert count and int(count[1])==5877

summary={**result,'block_checks':result['checks'],'arithmetic_checks':5877,'arithmetic_cases':5875,'cases':5938,'checks':result['checks']+5877,'reference_revision':b['reference_revision'],'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(corpus),'evidence_sha256':sha(evidence),'checker_sha256':sha(binary),'profile_sha256':sha(ROOT/'configs/private-hierarchy-v1.json'),'public_network':False,'global_header_admission':False,'history_verified_from_network':False}
save(out/'summary.json',summary);save(ROOT/'results/latest-weakshares.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'checks':result['checks']+5877,'executed_inbound_etxs':30},indent=2))
