#!/usr/bin/env python3
"""Bounded recent-cache, leader search and pending reapplication checks; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Coordinator verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('coordinator-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/coordinator-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/coordinator.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_coordinator_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    assert source['input_eligibility_sha256']==sha(ROOT/'fixtures/protocol/eligibility.json') and source['input_eligibility_source_sha256']==sha(ROOT/'fixtures/protocol/eligibility-source.json')
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-coordinator'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-coordinator'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/coordinator.json')],out/'coordinator.log'));assert result['passed'] and result['checks']==42731
    assert result['policy']['checks']==42267 and result['native']['checks']==464
    assert result['policy']['mismatches']==result['native']['mismatches']==0
    assert result['policy']['search_cases']==120 and result['policy']['accepted_searches']==40 and result['policy']['search_retries']==160
    assert result['policy']['sort_cases']==135 and result['policy']['recent_events']==2044 and result['policy']['reapplication_passes']==90
    assert result['native']['search_cases']==result['native']['accepted_searches']==16 and result['native']['native_recent_events']==9 and result['native']['reapplication_passes']==12
    assert result['native']['native_blocks']==6 and result['native']['context_component_bundles']==18
    assert result['native']['assembly_passed'] and result['native']['assembly_comparisons']==570
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/coordinator.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/coordinator-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'full_coordinator_qualified':False,'native_sync':False,'performance_measured':False,'library_candidate_scoring':True,'library_coordinator_projections':True,'library_global_eligibility_components':True,'library_coordinator_search_and_reapplication':True,'native_pending_header_generation':False,'scope':'Bounded recent-candidate admission/order, global pending-frontier search/retries, score-bound native reapplication, and virtual retry schedule. Controlled Go observation hooks, strict rejection boundaries and native-proof corpus are distinct. Generation workers, economic replay, canonical chain state and sync remain open.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-coordinator.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
