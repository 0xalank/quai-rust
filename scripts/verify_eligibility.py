#!/usr/bin/env python3
"""Bounded multi-context ancestry, constraints and PCRC checks; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Eligibility verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('eligibility-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/eligibility-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/eligibility.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_eligibility_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-eligibility'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-eligibility'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/eligibility.json')],out/'eligibility.log'));assert result['passed'] and result['checks']==14328
    assert result['policy']['checks']==13758 and result['native']['checks']==570
    assert result['policy']['mismatches']==result['native']['mismatches']==0
    assert result['policy']['safe_rejection_boundaries']==3
    assert result['native']['native_blocks']==6 and result['native']['context_component_bundles']==18
    assert result['native']['frontier_combinations']==343 and result['native']['eligible_frontiers']==7
    assert result['native']['native_trace_cases']==98 and result['native']['accepted_traces']==62
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/eligibility.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/eligibility-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'full_coordinator_qualified':False,'native_sync':False,'performance_measured':False,'library_candidate_scoring':True,'library_coordinator_projections':True,'library_global_eligibility_components':True,'scope':'Bounded imported multi-context ancestry, constraint tracing and PCRC frontier gates; six native prime-coincident blocks across two branches. Controlled policy projections and strict rejection boundaries are separate. No global leader search/retry loop, canonical economic history, durable state, fork-choice commitment or sync.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-eligibility.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
