#!/usr/bin/env python3
"""Explicit pinned-source incoming ETX oracle; never invoked by normal builds."""
import argparse,json,os,subprocess,tempfile,hashlib
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
source=(go/'core/state_processor.go').read_text();start=source.index('\t\tif tx.Type() == types.ExternalTxType {',source.index('func (p *StateProcessor) Process('));end=source.index('\n\t\t} else if tx.Type() == types.QuaiTxType',start);branch=source[start:end]+'\n\t\t}'
template=ROOT/'reference/incoming/core-template.go';main=ROOT/'reference/incoming/main.go';env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
with tempfile.TemporaryDirectory(prefix='quai-incoming-')as directory:
 d=Path(directory);adapter=d/'core-export.go';adapter.write_text(template.read_text().replace('/* OFFICIAL_INCOMING_DISPATCH */',branch));overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-incoming-vectors/main.go'):str(main),str(go/'core/native_incoming.go'):str(adapter),str(go/'core/native_incoming_harness.go'):str(ROOT/'reference/early-blocks/harness.go')}}));subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-incoming-vectors'],cwd=go,env=env,check=True);output=subprocess.check_output([str(binary)],cwd=d,env=env);(ROOT/'results/incoming-oracle-raw.log').write_bytes(output);v=json.loads(output.splitlines()[-1])
assert v['reference_revision']==PIN and len(v['vectors'])==429
# Location bytes are encoded as base64 by Go; expose the coordinate pair.
import base64
for row in v['vectors']:row['location']=list(base64.b64decode(row['location']))
path=ROOT/'fixtures/protocol/incoming.json';encoded=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode()
e={'reference_revision':PIN,'vectors_sha256':hashlib.sha256(encoded).hexdigest(),'oracle_source_sha256':sha(main),'template_sha256':sha(template),'dispatch_sha256':hashlib.sha256(branch.encode()).hexdigest(),'harness_sha256':sha(ROOT/'reference/early-blocks/harness.go'),'oracle_sources':{str(p.relative_to(ROOT)):sha(p)for p in [main,template,ROOT/'reference/early-blocks/harness.go']},'driver_sha256':sha(Path(__file__).resolve()),'go_sources':{name:sha(go/name)for name in ['core/state_processor.go','core/state_transition.go','core/state/statedb.go','core/state/state_object.go','core/vm/evm.go','core/vm/contracts.go','core/rawdb/accessors_chain.go','core/types/receipt.go','core/types/utxo.go','core/types/transaction.go','params/protocol_params.go','go.mod','go.sum']},'public_network':False,'runtime_go_dependency':False,'full_block_admission':False,'scope':v['scope']};target=ROOT/'fixtures/protocol/incoming-source.json'
if a.write:path.write_bytes(encoded);target.write_text(json.dumps(e,indent=2)+'\n')
else:
 if path.read_bytes()!=encoded or json.loads(target.read_text())!=e:raise SystemExit('Incoming ETX corpus/evidence differs')
print(json.dumps({'passed':True,'cases':len(v['vectors'])}))
