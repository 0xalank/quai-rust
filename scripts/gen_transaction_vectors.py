#!/usr/bin/env python3
"""Explicit pinned offline Go oracle; not called by the Rust build or runtime."""
import argparse,json,os
from pathlib import Path
import subprocess,tempfile
from lab import ROOT,save,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',default=os.environ.get('GO_QUAI_DIR',ROOT.parent/'go-quai'));a=p.parse_args()
go=Path(a.go_quai).resolve();revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=go,text=True).strip()
if revision!='33db82f2891b267ba898ca38bd2dd8bf75cbb072':raise SystemExit('Reference revision differs; use a separately versioned corpus')
if subprocess.check_output(['git','diff','HEAD','--','*.go','go.mod','go.sum'],cwd=go):raise SystemExit('Reference tracked sources changed')
source=ROOT/'reference/transactions/main.go';subprocess.run(['gofmt','-w',str(source)],check=True)
with tempfile.TemporaryDirectory(prefix='quai-tx-oracle-') as d:
    temporary=Path(d);overlay=temporary/'overlay.json';binary=temporary/'tx-vectors'
    overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-tx-vectors/main.go'):str(source)}}))
    subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+revision,'-o',str(binary),'./cmd/quai-tx-vectors'],cwd=go,check=True)
    value=json.loads(subprocess.check_output([str(binary)],cwd=go))
    save(ROOT/'fixtures/protocol/transactions.json',value)
    names=['core/types/transaction.go','core/types/transaction_signing.go','core/types/utxo.go','core/types/quai_tx.go','core/types/qi_tx.go','core/types/external_tx.go','core/types/proto_block.proto','common/address.go','common/types.go','crypto/crypto.go','core/state_processor.go','go.mod','go.sum']
    crypto_dependency=json.loads(subprocess.check_output(['go','list','-m','-json','github.com/btcsuite/btcd/btcec/v2'],cwd=go))
    dependency_root=Path(crypto_dependency['Dir'])
    dependency={'module':crypto_dependency['Path'],'version':crypto_dependency['Version'],'sum':crypto_dependency.get('Sum'),'sources':{n:sha(dependency_root/n) for n in ['schnorr/signature.go','schnorr/musig2/keys.go']}}
    save(ROOT/'fixtures/protocol/transactions-source.json',{'reference_revision':revision,'generator_sha256':sha(source),'vectors_sha256':sha(ROOT/'fixtures/protocol/transactions.json'),'sources':{n:sha(go/n) for n in names},'scope':value['scope'],'signature_dependency':dependency,'test_keys':'Deterministic public test keys; no production keys or network activity.'})
print('Generated pinned offline transaction vectors.')
