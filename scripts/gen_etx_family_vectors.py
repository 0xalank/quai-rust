#!/usr/bin/env python3
"""Explicit pinned Go oracle for activated etx-family hierarchy/state components."""
import argparse,hashlib,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',type=Path,required=True);a=p.add_mutually_exclusive_group(required=True);a.add_argument('--write',action='store_true');a.add_argument('--check',action='store_true');args=p.parse_args();go=args.go_quai.resolve();checked_reference(go)
helpers=[ROOT/'reference/activation/core-export.go',ROOT/'reference/confirmation/core-export.go',ROOT/'reference/admission/harness.go']
with tempfile.TemporaryDirectory(prefix='quai-etx-family-oracle-')as temporary:
 d=Path(temporary);overlay={str(go/'cmd/quai-etx-family-vectors/main.go'):str(ROOT/'reference/etx-family/main.go'),str(go/'core/native_etx-family.go'):str(ROOT/'reference/etx-family/core-export.go'),**{str(go/'core'/('native_etx-family_support_'+str(i)+'.go')):str(path)for i,path in enumerate(helpers)}};(d/'overlay.json').write_text(json.dumps({'Replace':overlay}));env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 with(ROOT/'results/etx-family-oracle-build.log').open('w')as log:subprocess.run(['go','build','-overlay',str(d/'overlay.json'),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(d/'oracle'),'./cmd/quai-etx-family-vectors'],cwd=go,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
 with(ROOT/'results/etx-family-oracle-raw.stderr').open('w')as log:raw=subprocess.check_output([str(d/'oracle')],cwd=d,env=env,stderr=log)
 (ROOT/'results/etx-family-oracle-raw.stdout').write_bytes(raw);v=json.loads(raw)
assert v['reference_revision']==PIN and len(v['branches'])==2 and not v['validated_from_genesis']and not v['canonical_authority']
for branch in v['branches']:
 assert len(branch['vectors'])==16 and len(branch['initial_etxs'])==6
 assert len(branch['negatives'])==123
assert sum(a['work_object']==b['work_object']for a,b in zip(v['branches'][0]['vectors'],v['branches'][1]['vectors']))==8
assert len({r['work_object']for b in v['branches']for r in b['vectors']})==24
encoded=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode();path=ROOT/'fixtures/protocol/etx-family.json';meta={'reference_revision':PIN,'vectors_sha256':hashlib.sha256(encoded).hexdigest(),'driver_sha256':sha(Path(__file__)),'oracle_sources':{str(n.relative_to(ROOT)):sha(n)for n in [*sorted((ROOT/'reference/etx-family').glob('*.go')),*helpers]},'go_sources':{n:sha(go/n)for n in ['core/headerchain_validation.go','core/headerchain.go','core/block_validator.go','core/state_processor.go','core/state_transition.go','core/slice.go','core/state/statedb.go','core/types/transaction_signing.go','core/types/utxo.go','core/rawdb/accessors_chain.go','core/evm.go','core/types/wo.go','consensus/progpow/progpow.go','consensus/progpow/algorithm.go','consensus/progpow/algorithm_progpow.go','consensus/misc/statefee.go','consensus/misc/rewards.go','params/protocol_params.go','go.mod','go.sum']},'profile_sha256':sha(ROOT/'configs/private-hierarchy-v1.json'),'scope':v['scope'],'synthetic_checkpoint':True,'checkpoint_height':259200,'branch_runs':32,'distinct_conditional_blocks':24,'valid_work_rejections':246,'validated_from_genesis':False,'canonical_authority':False,'runtime_go_dependency':False,'public_network':False};source=path.with_name('etx-family-source.json')
if args.write:path.write_bytes(encoded);source.write_text(json.dumps(meta,indent=2)+'\n')
else:assert path.read_bytes()==encoded and json.loads(source.read_text())==meta
print(json.dumps({'passed':True,'branch_runs':32,'distinct_conditional_blocks':24,'valid_work_rejections':246,'canonical_authority':False}))
