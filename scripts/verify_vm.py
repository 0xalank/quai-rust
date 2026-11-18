#!/usr/bin/env python3
"""Verify frozen VM vectors using native Rust only, without sockets or Go."""
import datetime, json, os, subprocess
from pathlib import Path
from lab import ROOT, sha, save

def run(command,log):
    result=subprocess.run(command,cwd=ROOT,text=True,capture_output=True)
    log.write_text(result.stdout+result.stderr)
    if result.returncode:raise SystemExit('Verification failed; see '+str(log))
    return result.stdout

def main():
    out=ROOT/'results'/('vm-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
    build=json.loads((ROOT/'results/rust-build.json').read_text())
    for source,digest in build['rust_sources'].items():
        if sha(ROOT/source)!=digest:raise SystemExit('Rebuild native source first: '+source)
    assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
    provenance=json.loads((ROOT/'fixtures/protocol/serial-vm-source.json').read_text())
    assert provenance['vectors_sha256']==sha(ROOT/'fixtures/protocol/serial-vm.json')
    assert provenance['generator_sha256']==sha(ROOT/'reference/serial-vm/main.go')
    assert provenance['driver_sha256']==sha(ROOT/'scripts/gen_vm_vectors.py')
    run(['cargo','build','--locked','--offline','--release','-p','quai-execution','--example','check-vm'],out/'build.log')
    target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve();binary=target/'release/examples/check-vm'
    check=json.loads(run([str(binary),str(ROOT/'fixtures/protocol/serial-vm.json')],out/'vm.log'));assert check['passed']
    assert check['supported_opcode_values_unobserved']==[]
    save(out/'vm.json',check)
    summary={'passed':True,'execution':check,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/serial-vm.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/serial-vm-source.json'),'checker_sha256':sha(binary),'runtime_go_dependency':False,'public_network':False,'full_evm':False,'full_transaction_execution':False,'full_consensus':False,'native_libp2p_sync':False,'performance_measured':False,'scope':check['scope']}
    save(out/'summary.json',summary);save(ROOT/'results/latest-vm.json',{'directory':str(out.relative_to(ROOT)),'passed':True})
    print(json.dumps({'passed':True,'directory':str(out),'checks':check['checks'],'vm_cases':check['vm_cases'],'state_gas_cases':check['state_gas_cases']},indent=2))
if __name__=='__main__':main()
