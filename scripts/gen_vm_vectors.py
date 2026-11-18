#!/usr/bin/env python3
"""Explicit offline oracle for bounded Quai VM frames; never part of the native build."""
import argparse, json, os, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True)
    mode=p.add_mutually_exclusive_group(required=True);mode.add_argument('--write',action='store_true');mode.add_argument('--check',action='store_true')
    a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
    env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
    generator=ROOT/'reference/serial-vm/main.go'
    with tempfile.TemporaryDirectory(prefix='quai-vm-oracle-') as directory:
        d=Path(directory);overlay=d/'overlay.json';binary=d/'vm-oracle'
        overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-serial-vm-vectors/main.go'):str(generator)}}))
        subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-serial-vm-vectors'],cwd=go,env=env,check=True)
        value=json.loads(subprocess.check_output([str(binary)],cwd=d,env=env))
    # Exercise committed and reverted storage, errors and both access-list costs.
    cases=value['vectors'];assert len({v['id'] for v in cases})==len(cases)
    assert any(v['before_root']!=v['after_root'] and v['error'] is None for v in cases)
    assert any(v['error']=='invalid_access_list' for v in cases)
    assert any(v['refund']>0 and v['error'] is None for v in cases)
    assert any(v['reverted'] for v in cases)
    assert all(v['before_root']==v['after_root'] for v in cases if v['error'] or v['reverted'])
    path=ROOT/'fixtures/protocol/serial-vm.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
    if a.write:path.write_bytes(encoded)
    elif path.read_bytes()!=encoded:raise SystemExit('VM vectors differ from the pinned oracle')
    sources=sorted({str(p.relative_to(go)) for directory in ['core/vm','core/state','trie','params'] for p in (go/directory).rglob('*.go') if not p.name.endswith('_test.go')} | {'common/big.go','go.mod','go.sum'})
    module=json.loads(subprocess.check_output(['go','list','-m','-json','modernc.org/mathutil'],cwd=go,env=env));assert module['Version']=='v1.6.0'
    math=Path(module['Dir'])
    metadata={'schema':1,'reference_revision':PIN,'generator_sha256':sha(generator),'driver_sha256':sha(Path(__file__).resolve()),'vectors_sha256':sha(path),'sources':{n:sha(go/n) for n in sources},'mathutil':{'version':module['Version'],'sum':module['Sum'],'sources':{n:sha(math/n) for n in ['binarylog.go','sqr.go','sqr_std.go','LICENSE']}},'scope':value['scope'],'go_version':subprocess.check_output(['go','version'],env=env,text=True).strip(),'public_network':False}
    target=ROOT/'fixtures/protocol/serial-vm-source.json'
    if a.write:target.write_text(json.dumps(metadata,indent=2)+'\n')
    elif json.loads(target.read_text())!=metadata:raise SystemExit('VM source evidence differs')
    print(json.dumps({'passed':True,'mode':'write' if a.write else 'check','vm_cases':len(cases),'state_gas_cases':len(value['state_gas'])}))
if __name__=='__main__':main()
