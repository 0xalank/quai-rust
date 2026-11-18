#!/usr/bin/env python3
"""Native signed auxiliary compatibility checks; no Go, sockets or signing keys."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Auxiliary verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('auxiliary-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/auxiliary-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/auxiliary.json');assert source['generator_sha256']==sha(ROOT/'reference/auxiliary/main.go');assert source['driver_sha256']==sha(ROOT/'scripts/gen_auxiliary_vectors.py')
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-auxiliary'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-auxiliary'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/auxiliary.json')],out/'auxiliary.log'));assert result['passed'] and result['full_kawpow_cases']==2
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/auxiliary.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/auxiliary-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'full_workshare_admission':False,'full_header_validation':False,'native_sync':False,'performance_measured':False,'scope':result['scope']}
    save(out/'summary.json',summary);save(ROOT/'results/latest-auxiliary.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
