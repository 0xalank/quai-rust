#!/usr/bin/env python3
"""Explicit, pinned actual-code fee/payout oracle; normal Rust builds never run Go."""
import argparse,base64,json,os,subprocess,tempfile,hashlib
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
source=(go/'core/state_processor.go').read_text();start=source.index('\t// Since the exchange rates are only calculated on prime blocks,');end=source.index('\n\ttime4 :=',start);branch=source[start:end];template=ROOT/'reference/payouts/core-template.go';main=ROOT/'reference/payouts/main.go';harness=ROOT/'reference/early-blocks/harness.go'
with tempfile.TemporaryDirectory(prefix='quai-payout-')as directory:
 d=Path(directory);adapter=d/'adapter.go';adapter.write_text(template.read_text().replace('/* OFFICIAL_FEE_PAYOUT_BRANCH */',branch));overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-payout-vectors/main.go'):str(main),str(go/'core/native_payout.go'):str(adapter),str(go/'core/native_payout_harness.go'):str(harness)}}));env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'};subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-payout-vectors'],cwd=go,env=env,check=True);output=subprocess.check_output([str(binary)],cwd=d,env=env);(ROOT/'results/block-payout-oracle-raw.log').write_bytes(output);v=json.loads(output.splitlines()[-1])
assert v['reference_revision']==PIN and len(v['vectors'])==486
for row in v['vectors']:row['location']=list(base64.b64decode(row['location']))
path=ROOT/'fixtures/protocol/payouts.json';encoded=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode();e={'reference_revision':PIN,'vectors_sha256':hashlib.sha256(encoded).hexdigest(),'dispatch_sha256':hashlib.sha256(branch.encode()).hexdigest(),'driver_sha256':sha(Path(__file__).resolve()),'oracle_sources':{str(p.relative_to(ROOT)):sha(p)for p in [main,template,harness]},'go_sources':{name:sha(go/name)for name in ['core/state_processor.go','core/headerchain.go','core/headerchain_validation.go','consensus/misc/rewards.go','params/protocol_params.go','core/types/auxpow_coinbase_utils.go','common/types.go','go.mod','go.sum']},'scope':v['scope'],'public_network':False,'full_consensus':False};target=path.with_name('payouts-source.json')
if a.write:path.write_bytes(encoded);target.write_text(json.dumps(e,indent=2)+'\n')
elif path.read_bytes()!=encoded or json.loads(target.read_text())!=e:raise SystemExit('Payout corpus differs')
print(json.dumps({'passed':True,'cases':len(v['vectors'])}))
