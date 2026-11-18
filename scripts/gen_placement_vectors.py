#!/usr/bin/env python3
"""Explicit offline oracle for bounded candidate placement and share controllers."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
    sources=['reference/placement/main.go','reference/placement/core-export.go','reference/consensus/core-export.go','reference/history/core-export.go','reference/header/core-export.go','reference/hierarchy/core-export.go']
    env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
    with tempfile.TemporaryDirectory(prefix='quai-placement-oracle-') as directory:
        d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-placement-vectors/main.go'):str(ROOT/sources[0]),str(go/'core/native_placement_vectors.go'):str(ROOT/sources[1]),str(go/'core/native_projection_vectors.go'):str(ROOT/sources[2]),str(go/'core/native_history_vectors.go'):str(ROOT/sources[3]),str(go/'core/native_header_vectors.go'):str(ROOT/sources[4]),str(go/'core/native_hierarchy_vectors.go'):str(ROOT/sources[5])}}))
        subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-placement-vectors'],cwd=go,env=env,check=True)
        value=json.loads(subprocess.check_output([str(binary)],cwd=d,env=env))
    assert value['reference_revision']==PIN
    expected={'orders':336,'updates':336,'append_commitments':72,'termini_codecs':48,'pcrc':756,'native':56}
    assert {k:len(value[k]) for k in expected}==expected
    assert {v['order'] for v in value['orders']}=={0,1,2}
    assert {v['rank'] for v in value['orders'] if v['rank'] is not None}==set(range(5))
    assert {k:sum(v['valid'] for v in value[k]) for k in ['append_commitments','termini_codecs','pcrc','native']}=={'append_commitments':16,'termini_codecs':36,'pcrc':630,'native':13}
    assert all(not v['returned_manifest'] for v in value['updates'])

    path=ROOT/'fixtures/protocol/placement.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
    names=['core/headerchain.go','core/core.go','core/slice.go','core/headerchain_validation.go','core/poem.go','core/types/wo.go','core/types/auxpow.go','params/protocol_params.go','common/big.go','common/types.go','core/types/block.go','core/block_validator.go','consensus/misc/rewards.go','consensus/misc/statefee.go','consensus/progpow/progpow.go','consensus/progpow/algorithm.go','consensus/kawpow/kawpow.go','consensus/kawpow/algorithm.go','consensus/kawpow/algorithm_kawpow.go','go.mod','go.sum']
    evidence={'schema':1,'reference_revision':PIN,'vectors_sha256':None,'oracle_sources':{n:sha(ROOT/n) for n in sources},'driver_sha256':sha(Path(__file__).resolve()),'sources':{n:sha(go/n) for n in names},'go_version':subprocess.check_output(['go','version'],env=env,text=True).strip(),'scope':value['scope'],'public_network':False,'runtime_go_dependency':False,'manifest_semantics':'Compare rawdb persisted next manifests. First-call CalculateManifest returns nil because of Go variable shadowing; returned_manifest records that separately. Stored empty manifests are unavailable to AppendHeader outside prime context.'}
    if a.write:
        tmp=path.with_suffix('.json.tmp');tmp.write_bytes(encoded);tmp.replace(path)
    elif path.read_bytes()!=encoded:raise SystemExit('Placement corpus differs from pinned reference')
    evidence['vectors_sha256']=sha(path);target=ROOT/'fixtures/protocol/placement-source.json'
    if a.write:target.write_text(json.dumps(evidence,indent=2)+'\n')
    elif json.loads(target.read_text())!=evidence:raise SystemExit('Placement source evidence differs')
    print(json.dumps({'passed':True,'mode':'write' if a.write else 'check','cases':{k:len(v) for k,v in value.items() if isinstance(v,list)}}))
if __name__=='__main__':main()
