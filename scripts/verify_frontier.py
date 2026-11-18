#!/usr/bin/env python3
"""Native bounded candidate append preparation; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Frontier verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('frontier-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/frontier-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/frontier.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_frontier_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-frontier'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-frontier'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/frontier.json')],out/'frontier.log'));assert result['passed'] and result['native']['attempts']==240 and result['native']['accepted']==54
    assert result['checks']==9988 and result['policy']['mismatches']==result['native']['mismatches']==0
    assert result['policy']['checks']==9054 and result['native']['checks']==934
    assert result['native']['constructed_startup_profiles']==4 and result['native']['frontier_updates']==54
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/frontier.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/frontier-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'full_coordinator_qualified':False,'native_sync':False,'performance_measured':False,'library_candidate_scoring':True,'library_coordinator_projections':True,'scope':'Constructed expanded-genesis native startup replay, local candidate PoEM scores and coordinator NodeSet/pending projections, with explicit legacy location-name aliasing. No allocations, durable state, global eligibility, canonical fork choice or sync.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-frontier.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
