#!/usr/bin/env python3
"""Explicit pinned Go oracle: sealed mixed blocks after a supplied checkpoint."""
import argparse,hashlib,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

p=argparse.ArgumentParser();p.add_argument('--go-quai',type=Path,required=True);a=p.add_mutually_exclusive_group(required=True);a.add_argument('--write',action='store_true');a.add_argument('--check',action='store_true');args=p.parse_args();go=args.go_quai.resolve();checked_reference(go)
with tempfile.TemporaryDirectory(prefix='quai-activation-oracle-')as temporary:
 d=Path(temporary);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-activation-vectors/main.go'):str(ROOT/'reference/activation/main.go'),str(go/'core/native_activation.go'):str(ROOT/'reference/activation/core-export.go'),str(go/'core/native_activation_harness.go'):str(ROOT/'reference/admission/harness.go')}}));env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 with(ROOT/'results/activation-oracle-build.log').open('w')as log:subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-activation-vectors'],cwd=go,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
 with(ROOT/'results/activation-oracle-raw.stderr').open('w')as log:raw=subprocess.check_output([str(binary)],cwd=d,env=env,stderr=log)
 (ROOT/'results/activation-oracle-raw.stdout').write_bytes(raw);v=json.loads(raw)
assert v['reference_revision']==PIN and len(v['vectors'])==4 and len(v['negatives'])==33 and not v['validated_from_genesis']and not v['canonical_authority']
assert [r['height']for r in v['vectors']]==[259201,259202,259203,259204]
encoded=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode();path=ROOT/'fixtures/protocol/activation.json';meta={'reference_revision':PIN,'vectors_sha256':hashlib.sha256(encoded).hexdigest(),'driver_sha256':sha(Path(__file__)),'oracle_sources':{str(n.relative_to(ROOT)):sha(n)for n in [*sorted((ROOT/'reference/activation').glob('*.go')),ROOT/'reference/admission/harness.go']},'go_sources':{n:sha(go/n)for n in ['core/headerchain_validation.go','core/headerchain.go','core/block_validator.go','core/state_processor.go','core/state_transition.go','core/slice.go','core/state/statedb.go','core/types/transaction_signing.go','core/types/utxo.go','core/rawdb/accessors_chain.go','core/evm.go','consensus/progpow/progpow.go','consensus/progpow/algorithm.go','consensus/progpow/algorithm_progpow.go','core/types/wo.go','consensus/misc/statefee.go','consensus/misc/rewards.go','params/protocol_params.go','go.mod','go.sum']},'profile_sha256':sha(ROOT/'configs/private-admission-v1.json'),'scope':v['scope'],'synthetic_checkpoint':True,'checkpoint_height':259200,'valid_blocks':4,'valid_work_rejections':33,'validated_from_genesis':False,'canonical_authority':False,'runtime_go_dependency':False,'public_network':False};source=path.with_name('activation-source.json')
if args.write:path.write_bytes(encoded);source.write_text(json.dumps(meta,indent=2)+'\n')
else:assert path.read_bytes()==encoded and json.loads(source.read_text())==meta
print(json.dumps({'passed':True,'valid_blocks':4,'valid_work_rejections':33,'checkpoint_height':259200,'validated_from_genesis':False,'canonical_authority':False}))
