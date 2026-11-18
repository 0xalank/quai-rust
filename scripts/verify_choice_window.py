#!/usr/bin/env python3
"""Qualify the branch-bound native token-choice window without Go execution."""
import datetime,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,sha,save
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for n,h in b[group].items():assert sha(ROOT/n)==h,n
corpus=ROOT/'fixtures/protocol/choice-window.json';source=corpus.with_name('choice-window-source.json');m=json.loads(source.read_text())
assert m['reference_revision']==b['reference_revision'] and m['vectors_sha256']==sha(corpus)
assert m['driver_sha256']==sha(ROOT/'scripts/gen_choice_window_vectors.py')
for n,h in m['oracle_sources'].items():assert sha(ROOT/n)==h,n
out=ROOT/'results'/('choice-window-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir()
shutil.copy2(Path(__file__),out/'qualification-driver.py')
with (out/'build.log').open('w') as log:
 subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-consensus','--example','check-choice-window'],cwd=ROOT,check=True,stdout=log,stderr=subprocess.STDOUT)
binary=out/'check-choice-window';shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/check-choice-window',binary)
with tempfile.TemporaryDirectory(prefix='quai-choice-window-verify-') as scratch:
 result=subprocess.run([str(binary),str(corpus),scratch],cwd=ROOT,capture_output=True,text=True,timeout=240,check=True)
(out/'native.log').write_text(result.stdout+result.stderr)
q=json.loads(result.stdout.splitlines()[-1]);assert q['passed'] and q['updates']==4216 and q['checks']==33728 and q['go_comparisons']==25296
assert q['go_window_entries_bound_by_matching_digests']==16864000 and q['writer_reopens']==8
assert q['aborted_or_unpublished_parent_rejections']==2 and q['fork_isolation_checks']==2
assert q['max_rows_copied_per_update']==64 and q['max_persisted_row_writes_per_update']==1 and q['metadata_writes_per_update']==1
assert not q['canonical_authority'] and not q['full_consensus'] and q['supplied_seed']
summary={**q,'build_sha256':sha(ROOT/'results/rust-build.json'),'reference_revision':b['reference_revision'],'corpus_sha256':sha(corpus),'source_sha256':sha(source),
 'checker_artifact':str(binary.relative_to(ROOT)),'checker_sha256':sha(binary),'qualification_driver_sha256':sha(Path(__file__)),
 'scope':m['scope'],'runtime_go_dependency':False,'evidence_files':{p.name:sha(p) for p in out.iterdir() if p.is_file()}}
save(out/'summary.json',summary);save(ROOT/'results/latest-choice-window.json',{'passed':True,'directory':str(out.relative_to(ROOT))})
print(json.dumps(q,indent=2))
