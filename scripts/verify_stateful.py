#!/usr/bin/env python3
"""Verify the frozen native stateful components without Go or public peers."""
import datetime,json,os,subprocess,re
from pathlib import Path
from lab import ROOT,sha,save
def main():
 build=json.loads((ROOT/'results/rust-build.json').read_text())
 for group in ['rust_sources','source_assets']:
  for name,digest in build[group].items():assert sha(ROOT/name)==digest,'Rebuild source first: '+name
 assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
 out=ROOT/'results'/('stateful-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
 target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve();reports={}
 names=['stateful','settlement','qi-settlement','early-blocks','incoming','historical']
 with (out/'build.log').open('w')as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-execution',*[arg for name in names for arg in ['--example','check-'+name]]],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
 for name in names:
  corpus=ROOT/'fixtures/protocol'/f'{name}.json';evidence=ROOT/'fixtures/protocol'/f'{name}-source.json';manifest=json.loads(evidence.read_text());assert manifest['reference_revision']==build['reference_revision'];assert manifest['vectors_sha256']==sha(corpus)
  driver='gen_stateful_vectors.py'if name=='stateful'else'gen_early_block_vectors.py'if name=='early-blocks'else'gen_'+name.replace('-','_')+'_vectors.py'
  assert manifest['driver_sha256']==sha(ROOT/'scripts'/driver)
  if 'oracle_sources'in manifest:
   for path,digest in manifest['oracle_sources'].items():assert sha(ROOT/path)==digest,path
  else:
   assert manifest['oracle_source_sha256']==sha(ROOT/'reference'/name/'main.go')
   if 'adapter_sha256'in manifest:assert manifest['adapter_sha256']==sha(ROOT/'reference'/name/'core-export.go')
   if 'harness_sha256'in manifest:assert manifest['harness_sha256']==sha(ROOT/'reference'/name/'harness.go')
  binary=target/'release/examples'/('check-'+name);result=subprocess.run([str(binary),str(corpus)],cwd=ROOT,text=True,capture_output=True);(out/(name+'.log')).write_text(result.stdout+result.stderr);assert result.returncode==0,'Native mismatch: '+name
  report=json.loads(result.stdout);assert report['passed'];save(out/(name+'.json'),report);reports[name]={'passed':True,'cases':report.get('cases',report.get('blocks')),'checks':report['checks'],'corpus_sha256':sha(corpus),'evidence_sha256':sha(evidence),'checker_sha256':sha(binary)}
 authority=ROOT/'fixtures/protocol/authority.json';manifest=json.loads(authority.with_name('authority-source.json').read_text());assert manifest['vectors_sha256']==sha(authority);assert manifest['driver_sha256']==sha(ROOT/'scripts/gen_authority_vectors.py');assert manifest['oracle_source_sha256']==sha(ROOT/'reference/authority/main.go')
 result=subprocess.run(['cargo','test','--locked','--offline','-p','quai-execution','authority_tests','--','--nocapture'],cwd=ROOT,text=True,capture_output=True);(out/'authority.log').write_text(result.stdout+result.stderr);assert result.returncode==0
 counts=re.search(r'AUTHORITY_CHECKS cases=(\d+) checks=(\d+)',result.stdout);assert counts;reports['authority']={'passed':True,'cases':int(counts[1]),'checks':int(counts[2]),'corpus_sha256':sha(authority),'evidence_sha256':sha(authority.with_name('authority-source.json'))}
 summary={'passed':True,'reference_revision':build['reference_revision'],'reference_version':'v0.57.0','build_sha256':sha(ROOT/'results/rust-build.json'),'components':reports,'private_profile_sha256':sha(ROOT/'configs/private-native-v1.json'),'runtime_go_dependency':False,'performance_measured':False,'full_evm':False,'full_economic_replay':False,'full_block_consensus':False,'native_public_sync':False,'scope':'Bounded nested execution, signed Quai settlement/actual applyTransaction finalization, ProcessQiTx UTXO/ETX settlement, and actual StateProcessor.Process on three early private blocks. Includes incoming ETX dispatch, lockup calls, authority updates and historical allocation/redemption/trim components. Reward-depth block assembly, complete economic replay and proof/hierarchy/canonical admission remain open.'}
 save(out/'summary.json',summary);save(ROOT/'results/latest-stateful.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'components':{n:{k:r[k]for k in ['cases','checks']}for n,r in reports.items()}},indent=2))
if __name__=='__main__':main()
