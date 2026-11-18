#!/usr/bin/env python3
"""Explicit offline oracle for signed auxiliary admission; not part of runtime/build."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
    source=ROOT/'reference/auxiliary/main.go';env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
    with tempfile.TemporaryDirectory(prefix='quai-auxiliary-oracle-')as directory:
        d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-auxiliary-vectors/main.go'):str(source)}}))
        subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-auxiliary-vectors'],cwd=go,env=env,check=True)
        value=json.loads(subprocess.check_output([str(binary)],cwd=d,env=env))
    assert value['reference_revision']==PIN
    assert len(value['profiles'])==len(value['golden'])==2
    assert all(v['valid'] for v in value['golden'])
    assert all(next(v for v in value['templates'] if v['id']==key)['signature_valid'] for key in ['pinned-1-0','pinned-3-0','pinned-4-0'])
    path=ROOT/'fixtures/protocol/auxiliary.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
    if a.write:
        temporary=path.with_suffix('.json.tmp');temporary.write_bytes(encoded);temporary.replace(path)
    elif path.read_bytes()!=encoded:raise SystemExit('Auxiliary corpus differs from pinned reference')
    names=['core/types/auxpow.go','core/types/auxpow_coinbase_utils.go','core/types/ravencoin.go','core/types/btc.go','core/types/bch.go','core/types/ltc.go','core/types/wo.go','core/types/proto_block.proto','core/headerchain_validation.go','core/headerchain.go','crypto/musig2/musig2.go','params/protocol_params.go','go.mod','go.sum']
    names+=['consensus/kawpow/'+n for n in ['kawpow.go','algorithm.go','algorithm_kawpow.go']]
    evidence={'schema':1,'reference_revision':PIN,'vectors_sha256':sha(path),'generator_sha256':sha(source),'driver_sha256':sha(Path(__file__).resolve()),'sources':{n:sha(go/n)for n in names},'go_version':subprocess.check_output(['go','version'],env=env,text=True).strip(),'scope':value['scope'],'keys':'Pinned network public keys and the deterministic public lab keyset (private scalar 1, 2, 3) only. Lab trust is explicit and signatures fail the pinned runtime keyset. No wallets, public peers or transactions.','public_network':False,'runtime_go_dependency':False}
    target=ROOT/'fixtures/protocol/auxiliary-source.json'
    if a.write:target.write_text(json.dumps(evidence,indent=2)+'\n')
    elif json.loads(target.read_text())!=evidence:raise SystemExit('Auxiliary source evidence differs')
    print(json.dumps({'passed':True,'mode':'write'if a.write else'check','template_cases':len(value['templates']),'binding_cases':len(value['auxiliaries']),'golden_kawpow':len(value['golden'])}))
if __name__=='__main__':main()
