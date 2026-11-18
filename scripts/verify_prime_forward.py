#!/usr/bin/env python3
"""Verify frozen native linked prime forward outputs and atomic token windows; Go is not invoked."""
import datetime,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,sha,save
build=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in build[group].items():assert sha(ROOT/name)==digest,name
corpus=ROOT/'fixtures/protocol/prime-forward.json';source=corpus.with_name('prime-forward-source.json')
m=json.loads(source.read_text());assert m['reference_revision']==build['reference_revision'] and m['vectors_sha256']==sha(corpus)
assert m['driver_sha256']==sha(ROOT/'scripts/gen_prime_forward_vectors.py')
for name,digest in m['oracle_sources'].items():assert sha(ROOT/name)==digest,name
out=ROOT/'results'/('prime-forward-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir()
shutil.copy2(Path(__file__),out/'qualification-driver.py')
with (out/'build.log').open('w') as log:
 subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-prime-forward'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
binary=out/'check-prime-forward';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-prime-forward',binary)
with tempfile.TemporaryDirectory(prefix='quai-prime-forward-proof-') as scratch:
 result=subprocess.run([str(binary),str(corpus),str(Path(scratch)/'state')],cwd=ROOT,capture_output=True,text=True,check=True)
(out/'native.log').write_text(result.stdout+result.stderr);q=json.loads(result.stdout)
assert q['passed'] and q['linked_forward_steps']==1024 and q['histories']==256
assert q['output_sequence_comparisons']==q['output_root_comparisons']==1024
assert q['choice_window_digest_comparisons']==q['choice_sum_comparisons']==q['forward_rate_comparisons']==1024
assert q['matched_etx_encodings']==23040 and q['reference_refunds']==10240
assert q['native_missing_inputs_rejected']==768 and q['native_bindings_rejected']==1024
assert q['reference_undefined_zero_cases']==q['native_undefined_zero_cases_refused']==240
assert q['persisted_forward_updates']==32 and q['writer_reopens']==16 and q['atomic_aborts']==8
assert q['max_window_rows_copied']==64 and q['max_persisted_row_writes']==1
assert q['supplied_seed'] and not q['canonical_authority'] and not q['full_consensus']
summary={**q,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':build['reference_revision'],
 'corpus_sha256':sha(corpus),'source_sha256':sha(source),'checker_artifact':str(binary.relative_to(ROOT)),'checker_sha256':sha(binary),
 'qualification_driver_sha256':sha(Path(__file__)),'timing_measured':False,'heap_or_rss_measured':False,
 'scope':m['scope'],'evidence_files':{p.name:sha(p) for p in out.iterdir() if p.is_file()}}
save(out/'summary.json',summary);save(ROOT/'results/latest-prime-forward.json',{'passed':True,'directory':str(out.relative_to(ROOT))})
print(json.dumps(q,indent=2))
