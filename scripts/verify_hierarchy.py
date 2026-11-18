#!/usr/bin/env python3
"""Native bounded prime/region header verification; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Hierarchy verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('hierarchy-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/hierarchy-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/hierarchy.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_hierarchy_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-hierarchy'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-hierarchy'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/hierarchy.json')],out/'hierarchy.log'));assert result['passed'] and result['native']['cases']==66 and result['native']['accepted_component_bundles']==21
    assert all(n>0 for n in result['policy']['parent_orders']) and result['native']['accepted_with_zero_context_parent']==12
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/hierarchy.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/hierarchy-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'candidate_hierarchy_placement_qualified':False,'native_sync':False,'performance_measured':False,'scope':'Bounded prime/region header policy, efficiency/threshold wrapping, miner-difficulty EMA, ETX eligibility; actual native parent/candidate proof/body component fixtures on supplied candidate history. No economic replay, fork choice or canonical authority.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-hierarchy.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
