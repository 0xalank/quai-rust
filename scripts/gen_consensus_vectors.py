#!/usr/bin/env python3
"""Generate explicit offline consensus kernel/rule vectors, separate from native runtime."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args()
    go=Path(a.go_quai).resolve();checked_reference(go);env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'};ref=ROOT/'reference/consensus'
    with tempfile.TemporaryDirectory(prefix='quai-consensus-oracle-')as directory:
        d=Path(directory);mapping={str(go/'cmd/quai-native-consensus/main.go'):str(ref/'main.go'),str(go/'core/native_vectors.go'):str(ref/'core-export.go')}
        for pkg in ['progpow','kawpow']:
            source=d/(pkg+'-export.go');source.write_text((ref/'pow-export.go').read_text().replace('POWPACKAGE',pkg).replace('POWKERNEL',pkg+'Light'));mapping[str(go/f'consensus/{pkg}/native_vectors.go')]=str(source)
        overlay=d/'overlay.json';overlay.write_text(json.dumps({'Replace':mapping}));binary=d/'oracle'
        subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-native-consensus'],cwd=go,env=env,check=True)
        value=json.loads(subprocess.check_output([str(binary)],cwd=d,env=env))
    assert value['schema']==1 and value['reference_revision']==PIN and len(value['pow_groups'])==5
    assert all('error' not in v['expected'] for v in value['entropy'])
    assert sum(len(g['objects']) for g in value['pow_groups'])==4
    path=ROOT/'fixtures/protocol/consensus.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
    if a.write:
        temporary=path.with_suffix('.json.tmp');temporary.write_bytes(encoded);temporary.replace(path)
    elif path.read_bytes()!=encoded:raise SystemExit('Consensus corpus differs from pinned reference')
    paths=sorted({str(p.relative_to(go)) for folder in ['consensus','core/types'] for p in (go/folder).rglob('*.go') if not p.name.endswith('_test.go')}|{'core/headerchain.go','core/headerchain_validation.go','core/poem.go','core/block_validator.go','common/big.go','common/types.go','params/protocol_params.go','go.mod','go.sum','consensus/kawpow/real_blocks_test.go'})
    metadata={'schema':1,'reference_revision':PIN,'vectors_sha256':sha(path),'generator_sources':{n:sha(ref/n)for n in ['main.go','core-export.go','pow-export.go']},'driver_sha256':sha(Path(__file__).resolve()),'go_sources':{n:sha(go/n)for n in paths},'scope':value['scope'],'arithmetic_oracle':'Actual core methods with a controlled hash supplied only for arithmetic projections. Kernel vectors use actual full-size generated caches/DAG items. No projected header is claimed consensus-valid.','public_network':False,'runtime_go_dependency':False}
    target=ROOT/'fixtures/protocol/consensus-source.json'
    if a.write:target.write_text(json.dumps(metadata,indent=2)+'\n')
    elif json.loads(target.read_text())!=metadata:raise SystemExit('Consensus source evidence differs')
    print(json.dumps({'passed':True,'mode':'write' if a.write else 'check','pow_inputs':sum(len(g['vectors'])for g in value['pow_groups']),'header_objects':4,'difficulty_cases':len(value['difficulty']),'entropy_cases':len(value['entropy'])}))
if __name__=='__main__':main()
