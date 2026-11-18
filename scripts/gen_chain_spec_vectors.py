#!/usr/bin/env python3
"""Explicit offline Go oracle for the chain spec and fork schedule (go-quai params)."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True)
 mode=p.add_mutually_exclusive_group(required=True);mode.add_argument('--write',action='store_true');mode.add_argument('--check',action='store_true');a=p.parse_args()
 go=Path(a.go_quai).resolve();checked_reference(go)
 names=['reference/chain-spec/main.go','reference/chain-spec/core-export.go']
 env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 with tempfile.TemporaryDirectory(prefix='quai-chain-spec-oracle-') as directory:
  d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle'
  overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-chain-spec-vectors/main.go'):str(ROOT/names[0]),str(go/'core/native_chain_spec_vectors.go'):str(ROOT/names[1])}}))
  subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-chain-spec-vectors'],cwd=go,env=env,check=True)
  value=json.loads(subprocess.run([str(binary)],cwd=d,env=env,capture_output=True,check=True).stdout)
 assert value['reference_revision']==PIN
 c=value['colosseum'];assert c['genesis_hash']==c['rebuilt_genesis_hash'],'Go rebuilt a different colosseum genesis'
 corpus=ROOT/'fixtures/protocol/chain-spec.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
 sources=['params/protocol_params.go','params/config.go','core/genesis.go','core/types/utxo.go','core/headerchain_validation.go','cmd/utils/flags.go','common/types.go','go.mod','go.sum']
 evidence={'reference_revision':PIN,'driver_sha256':sha(Path(__file__).resolve()),'oracle_sources':{n:sha(ROOT/n) for n in names},'sources':{n:sha(go/n) for n in sources},'scope':value['scope'],'runtime_go_dependency':False,'go_version':subprocess.check_output(['go','version'],env=env,text=True).strip()}
 if a.write:corpus.write_bytes(encoded)
 elif corpus.read_bytes()!=encoded:raise SystemExit('Chain-spec corpus differs from the pinned reference')
 evidence['vectors_sha256']=sha(corpus)
 source=corpus.with_name('chain-spec-source.json')
 if a.write:source.write_text(json.dumps(evidence,indent=2)+'\n')
 elif json.loads(source.read_text())!=evidence:raise SystemExit('Chain-spec source evidence differs')
 print(json.dumps({'passed':True,'mode':'write' if a.write else 'check','forks':len(value['forks']),'params':len(value['params'])}))
if __name__=='__main__':main()
