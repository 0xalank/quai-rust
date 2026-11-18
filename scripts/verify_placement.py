#!/usr/bin/env python3
"""Native bounded candidate append preparation; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Placement verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('placement-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/placement-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/placement.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_placement_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-placement'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-placement'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/placement.json')],out/'placement.log'));assert result['passed'] and result['native']['cases']==56 and result['native']['accepted']==13
    assert result['checks']==5478 and result['policy']['mismatches']==result['native']['mismatches']==0
    assert all(n>0 for n in result['policy']['orders']) and all(n>0 for n in result['policy']['ranks'])
    assert result['policy']['accepted_append']==16 and result['policy']['accepted_pcrc']==630 and result['policy']['accepted_termini_codecs']==36
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/placement.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/placement-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'candidate_hierarchy_placement_qualified':False,'native_sync':False,'performance_measured':False,'library_candidate_append_components':True,'scope':'Bounded candidate order/rank/routing, append manifest/interlink commitments and PCRC transitions, composed with native proof/header/body components on supplied unverified history. No economic replay, canonical publication, fork choice or network qualification.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-placement.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
