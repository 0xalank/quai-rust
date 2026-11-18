#!/usr/bin/env python3
"""Native bounded candidate append preparation; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Genesis verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('genesis-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/genesis-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/genesis.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_genesis_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-genesis'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-genesis'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/genesis.json')],out/'genesis.log'));assert result['passed'] and result['native']['cases']==120 and result['native']['accepted']==30
    assert result['checks']==2526 and result['policy']['mismatches']==result['native']['mismatches']==0
    assert result['policy']['checks']==2156 and result['policy']['wide_difficulty_cases']==19 and result['policy']['invalid_entropy_cases']==19
    assert result['native']['branch_profiles']==5
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/genesis.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/genesis-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'genesis_expansion_qualified':False,'native_sync':False,'performance_measured':False,'library_genesis_construction':True,'library_candidate_branch_storage':True,'scope':'Bounded canonical genesis construction, explicit candidate seeding, geometry and genesis difficulty arithmetic, and native multi-block competing candidate branch insertion. No allocation replay, fork choice, durable canonical publication or native sync.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-genesis.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
