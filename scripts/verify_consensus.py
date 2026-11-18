#!/usr/bin/env python3
"""Native consensus component acceptance against frozen vectors; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,path):
    result=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);path.write_text(result.stdout+result.stderr)
    if result.returncode:raise SystemExit('Consensus verification failed; see '+str(path))
    return result.stdout

def main():
    out=ROOT/'results'/('consensus-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text())
    assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    provenance=json.loads((ROOT/'fixtures/protocol/consensus-source.json').read_text())
    assert provenance['vectors_sha256']==sha(ROOT/'fixtures/protocol/consensus.json')
    assert provenance['driver_sha256']==sha(ROOT/'scripts/gen_consensus_vectors.py')
    for rel,h in provenance['generator_sources'].items():assert sha(ROOT/'reference/consensus'/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-consensus'],out/'build.log')
    target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve();binary=target/'release/examples/check-consensus'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/consensus.json')],out/'consensus.log'));assert result['passed']
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['pow']['checks']+result['rules']['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/consensus.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/consensus-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'full_header_validation':False,'full_auxiliary_proof_validation':False,'full_transaction_execution':False,'native_sync':False,'performance_measured':False,'scope':'Native light kernels and header/body binding plus exact rule projections. Full block admission explicitly unavailable; no chain authority or network qualification.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-consensus.json',{'directory':str(out.relative_to(ROOT)),'passed':True})
    print(json.dumps({'passed':True,'checks':summary['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
