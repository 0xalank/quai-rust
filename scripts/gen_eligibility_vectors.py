#!/usr/bin/env python3
"""Explicit offline oracle for bounded global coordinator ancestry, constraints and PCRC."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
    sources=['reference/eligibility/main.go','reference/eligibility/core-export.go','reference/consensus/core-export.go','reference/history/core-export.go','reference/header/core-export.go','reference/hierarchy/core-export.go','reference/placement/core-export.go','reference/genesis/core-export.go','reference/frontier/coordinator-export.go','reference/frontier/core-export.go','reference/eligibility/coordinator-export.go']
    env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
    with tempfile.TemporaryDirectory(prefix='quai-eligibility-oracle-') as directory:
        d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-eligibility-vectors/main.go'):str(ROOT/sources[0]),str(go/'core/native_eligibility_vectors.go'):str(ROOT/sources[1]),str(go/'core/native_projection_vectors.go'):str(ROOT/sources[2]),str(go/'core/native_history_vectors.go'):str(ROOT/sources[3]),str(go/'core/native_header_vectors.go'):str(ROOT/sources[4]),str(go/'core/native_hierarchy_vectors.go'):str(ROOT/sources[5]),str(go/'core/native_placement_vectors.go'):str(ROOT/sources[6]),str(go/'core/native_genesis_vectors.go'):str(ROOT/sources[7]),str(go/'cmd/utils/native_frontier_vectors.go'):str(ROOT/sources[8]),str(go/'core/native_frontier_vectors.go'):str(ROOT/sources[9]),str(go/'cmd/utils/native_eligibility_vectors.go'):str(ROOT/sources[10])}}))
        subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-eligibility-vectors'],cwd=go,env=env,check=True)
        value=json.loads(subprocess.check_output([str(binary)],cwd=d,env=env))
    assert value['reference_revision']==PIN
    policy=value['policy'];native=value['native']
    counts={k:len(policy[k]) for k in ['datasets','ancestors','pcrc','traces','boundaries']}
    assert counts=={'datasets':12,'ancestors':3960,'pcrc':648,'traces':1512,'boundaries':3}
    assert len(native['steps'])==6 and sum(len(s['contexts']) for s in native['steps'])==18
    assert len(native['matrix'])==343 and sum(r['valid'] for r in native['matrix'])==7
    assert len(native['traces'])==98 and sum(r['valid'] for r in native['traces'])==62
    assert len(native['entries'])==21
    assert all(r['go_success'] and r['strict_reject'] for r in policy['boundaries'])
    path=ROOT/'fixtures/protocol/eligibility.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
    names=['cmd/utils/hierarchical_coordinator.go','common/address.go','params/denomination.go','core/types/proto_block.proto','common/proto_common.proto','core/genesis.go','core/rawdb/accessors_chain.go','core/types/transaction.go','core/headerchain.go','core/core.go','core/slice.go','core/headerchain_validation.go','core/poem.go','core/types/wo.go','core/types/auxpow.go','params/protocol_params.go','common/big.go','common/types.go','core/types/block.go','core/block_validator.go','consensus/misc/rewards.go','consensus/misc/statefee.go','consensus/progpow/progpow.go','consensus/progpow/algorithm.go','consensus/kawpow/kawpow.go','consensus/kawpow/algorithm.go','consensus/kawpow/algorithm_kawpow.go','go.mod','go.sum']
    evidence={'schema':1,'reference_revision':PIN,'vectors_sha256':None,'oracle_sources':{n:sha(ROOT/n) for n in sources},'driver_sha256':sha(Path(__file__).resolve()),'sources':{n:sha(go/n) for n in names},'go_version':subprocess.check_output(['go','version'],env=env,text=True).strip(),'scope':value['scope'],'public_network':False,'runtime_go_dependency':False}
    if a.write:
        tmp=path.with_suffix('.json.tmp');tmp.write_bytes(encoded);tmp.replace(path)
    elif path.read_bytes()!=encoded:raise SystemExit('Eligibility corpus differs from pinned reference')
    evidence['vectors_sha256']=sha(path);target=ROOT/'fixtures/protocol/eligibility-source.json'
    if a.write:target.write_text(json.dumps(evidence,indent=2)+'\n')
    elif json.loads(target.read_text())!=evidence:raise SystemExit('Eligibility source evidence differs')
    print(json.dumps({'passed':True,'mode':'write' if a.write else 'check','policy_cases':counts,'native_blocks':6,'context_component_bundles':18,'frontier_combinations':343,'eligible_frontiers':7,'native_traces':98}))
if __name__=='__main__':main()
