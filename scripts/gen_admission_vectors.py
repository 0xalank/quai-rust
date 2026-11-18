#!/usr/bin/env python3
"""Explicit official-source zone whole-block execution oracle."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
source=ROOT/'reference/admission/main.go';env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
with tempfile.TemporaryDirectory(prefix='quai-admission-')as directory:
 d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-admission-vectors/main.go'):str(source),str(go/'core/native_admission.go'):str(ROOT/'reference/admission/core-export.go'),str(go/'core/native_admission_harness.go'):str(ROOT/'reference/admission/harness.go'),str(go/'core/native_admission_negative.go'):str(ROOT/'reference/admission/negative.go')}}));subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-admission-vectors'],cwd=go,env=env,check=True);output=subprocess.check_output([str(binary)],cwd=d,env=env);(ROOT/'results/admission-oracle-raw.log').write_bytes(output);v=json.loads(output.splitlines()[-1])
assert v['reference_revision']==PIN and len(v['branches'])==2 and all(len(b['vectors'])==16 for b in v['branches'])
assert v['branches'][0]['vectors'][:8]==v['branches'][1]['vectors'][:8]
assert all(a['block_hash']!=b['block_hash'] for a,b in zip(v['branches'][0]['vectors'][8:],v['branches'][1]['vectors'][8:]))
assert len(v['negatives'])==31 and all(not n['accepted']for n in v['negatives'])
profile=json.loads((ROOT/'configs/private-native-v1.json').read_text());profile['name']='quai-rust-admission-v1';profile['kind']='private-native-local-zone-admission';profile['genesis']['difficulty']='2';profile['genesis']['extra']='0x'+b'quai-rust-admission-v1'.hex()
for c in profile['genesis']['contexts']:c['hash']=v['genesis_hash'];c['body_hash']=v['genesis_body_hash']
profile['admission_scope']={'zone_local_only':True,'max_zone_height':259200,'new_inbound_etxs':'derived-empty-without-dominant-confirmation','minimum_difficulty':'2','duration_seconds':5,'p2p_threshold':7}
profile_path=ROOT/'configs/private-admission-v1.json';profile_bytes=(json.dumps(profile,indent=2)+'\n').encode()
if a.write:profile_path.write_bytes(profile_bytes)
elif profile_path.read_bytes()!=profile_bytes:raise SystemExit('Admission profile differs')
path=ROOT/'fixtures/protocol/admission.json';encoded=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode()
if a.write:path.write_bytes(encoded)
elif path.read_bytes()!=encoded:raise SystemExit('Zone block corpus differs')
e={'reference_revision':PIN,'vectors_sha256':sha(path),'profile_sha256':sha(profile_path),'oracle_source_sha256':sha(source),'adapter_sha256':sha(ROOT/'reference/admission/core-export.go'),'harness_sha256':sha(ROOT/'reference/admission/harness.go'),'negative_sha256':sha(ROOT/'reference/admission/negative.go'),'driver_sha256':sha(Path(__file__).resolve()),'go_sources':{str(p.relative_to(go)):sha(p)for name in ['core/poem.go','core/headerchain.go','core/block_validator.go','core/slice.go','consensus/progpow/progpow.go','consensus/progpow/algorithm.go','core/state_processor.go','core/headerchain_validation.go','core/evm.go','core/state_transition.go','core/state/statedb.go','core/state/state_object.go','core/state/gen_allocs.go','core/rawdb/accessors_chain.go','core/types/receipt.go','core/types/utxo.go','core/types/transaction_signing.go','core/types/transaction.go','consensus/misc/rewards.go','params/protocol_params.go','go.mod','go.sum']for p in [go/name]},'public_network':False,'runtime_go_dependency':False,'scope':v['scope']};target=ROOT/'fixtures/protocol/admission-source.json'
if a.write:target.write_text(json.dumps(e,indent=2)+'\n')
elif json.loads(target.read_text())!=e:raise SystemExit('Zone block source evidence differs')
print(json.dumps({'passed':True,'branches':2,'rows':32,'unique_blocks':24,'negative_cases':31}))
