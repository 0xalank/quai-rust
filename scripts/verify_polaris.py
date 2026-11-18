#!/usr/bin/env python3
"""Upstream Polaris binding, identity and inclusion-gate checks; no Go or sockets."""
import datetime,json,os,subprocess
from pathlib import Path
from lab import ROOT,sha,save

def run(command,log):
    r=subprocess.run(command,cwd=ROOT,text=True,capture_output=True);log.write_text(r.stdout+r.stderr)
    if r.returncode:raise SystemExit('Polaris verification failed; see '+str(log))
    return r.stdout

def main():
    out=ROOT/'results'/('polaris-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text());assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    for rel,h in build['rust_sources'].items():assert sha(ROOT/rel)==h,rel
    source=json.loads((ROOT/'fixtures/protocol/polaris-source.json').read_text());assert source['vectors_sha256']==sha(ROOT/'fixtures/protocol/polaris.json');assert source['driver_sha256']==sha(ROOT/'scripts/gen_polaris_vectors.py')
    for rel,h in source['oracle_sources'].items():assert sha(ROOT/rel)==h,rel
    run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-polaris'],out/'build.log')
    binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()/'release/examples/check-polaris'
    result=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/polaris.json')],out/'polaris.log'));assert result['passed'] and result['checks']==198
    assert result['bindings']==50 and result['identities']==28 and result['polaris_prime_block']==2444574
    assert result['reference_revision']=='33db82f2891b267ba898ca38bd2dd8bf75cbb072'
    save(out/'components.json',result)
    summary={'passed':True,'checks':result['checks'],'components':result,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/polaris.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/polaris-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_consensus':False,'canonical_history_verified':False,'full_coordinator_qualified':False,'native_sync':False,'performance_measured':False,'library_candidate_scoring':True,'library_coordinator_projections':True,'library_polaris_binding_and_identity_rules':True,'reference_revision':'33db82f2891b267ba898ca38bd2dd8bf75cbb072','reference_version':'v0.57.0','scope':'Official v0.57.0 all-height body/auxiliary binding, controlled order, donor proof identity and fork-gated duplicate-inclusion observations. Full economic replay and canonical admission remain unavailable.'}
    save(out/'summary.json',summary);save(ROOT/'results/latest-polaris.json',{'directory':str(out.relative_to(ROOT)),'passed':True});print(json.dumps({'passed':True,'checks':result['checks'],'directory':str(out),'full_consensus':False},indent=2))
if __name__=='__main__':main()
