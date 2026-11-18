#!/usr/bin/env python3
"""Explicit official-source zone whole-block execution oracle."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
source=ROOT/'reference/private-branches/main.go';env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
with tempfile.TemporaryDirectory(prefix='quai-private-branches-')as directory:
 d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-private-branches-vectors/main.go'):str(source),str(go/'core/native_private_branches.go'):str(ROOT/'reference/private-branches/core-export.go'),str(go/'core/native_zone_harness.go'):str(ROOT/'reference/early-blocks/harness.go')}}));subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-private-branches-vectors'],cwd=go,env=env,check=True);output=subprocess.check_output([str(binary)],cwd=d,env=env);(ROOT/'results/private-branches-oracle-raw.log').write_bytes(output);v=json.loads(output.splitlines()[-1])
assert v['reference_revision']==PIN and len(v['branches'])==2 and all(len(b['vectors'])==16 for b in v['branches'])
assert v['branches'][0]['vectors'][:8]==v['branches'][1]['vectors'][:8]
assert all(a['block_hash']!=b['block_hash'] for a,b in zip(v['branches'][0]['vectors'][8:],v['branches'][1]['vectors'][8:]))
path=ROOT/'fixtures/protocol/private-branches.json';encoded=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode()
if a.write:path.write_bytes(encoded)
elif path.read_bytes()!=encoded:raise SystemExit('Zone block corpus differs')
e={'reference_revision':PIN,'vectors_sha256':sha(path),'oracle_source_sha256':sha(source),'adapter_sha256':sha(ROOT/'reference/private-branches/core-export.go'),'harness_sha256':sha(ROOT/'reference/early-blocks/harness.go'),'driver_sha256':sha(Path(__file__).resolve()),'go_sources':{str(p.relative_to(go)):sha(p)for name in ['core/state_processor.go','core/headerchain_validation.go','core/evm.go','core/state_transition.go','core/state/statedb.go','core/state/state_object.go','core/state/gen_allocs.go','core/rawdb/accessors_chain.go','core/types/receipt.go','core/types/utxo.go','core/types/transaction_signing.go','core/types/transaction.go','consensus/misc/rewards.go','params/protocol_params.go','go.mod','go.sum']for p in [go/name]},'public_network':False,'runtime_go_dependency':False,'scope':v['scope']};target=ROOT/'fixtures/protocol/private-branches-source.json'
if a.write:target.write_text(json.dumps(e,indent=2)+'\n')
elif json.loads(target.read_text())!=e:raise SystemExit('Zone block source evidence differs')
print(json.dumps({'passed':True,'branches':2,'rows':32,'unique_blocks':24}))
