#!/usr/bin/env python3
"""Native bounded share-controller and ancestry verification; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('History verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('history-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/history-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/history.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_history_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-history'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-history'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/history.json')],out/'history.log'));assert result['passed'] and result['native']['cases']==48 and result['native']['accepted']==4
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/history.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/history-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'full_header_validation':False,'native_sync':False,'performance_measured':False,'scope':'Bounded controller/history policy projections plus actual native signed KAWPOW inclusion checks on imported candidate ancestry. No canonical publication or full block validity.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-history.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
