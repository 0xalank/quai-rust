#!/usr/bin/env python3
"""Explicit offline differential oracle for prime controller arithmetic."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True)
 mode=p.add_mutually_exclusive_group(required=True);mode.add_argument('--write',action='store_true');mode.add_argument('--check',action='store_true');a=p.parse_args()
 go=Path(a.go_quai).resolve();checked_reference(go)
 names=['reference/controllers/main.go','reference/controllers/core-export.go']
 env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 out=ROOT/'results';out.mkdir(exist_ok=True)
 with tempfile.TemporaryDirectory(prefix='quai-controller-oracle-') as directory:
  d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle'
  overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-controller-vectors/main.go'):str(ROOT/names[0]),str(go/'core/native_controller_vectors.go'):str(ROOT/names[1])}}))
  with (out/'controller-oracle-build.log').open('w') as log:
   subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-controller-vectors'],cwd=go,env=env,check=True,stdout=log,stderr=subprocess.STDOUT)
  result=subprocess.run([str(binary)],cwd=d,env=env,capture_output=True,check=True)
  (out/'controller-oracle.stdout').write_bytes(result.stdout);(out/'controller-oracle.stderr').write_bytes(result.stderr)
  value=json.loads(result.stdout)
 assert value['reference_revision']==PIN
 assert all(len(value[k])==640 for k in ['flow','miner_difficulty','discounts','updates','exchange_rates'])
 corpus=ROOT/'fixtures/protocol/controllers.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
 sources=['core/exchange_controller.go','consensus/misc/rewards.go','common/big.go','core/headerchain.go','params/protocol_params.go','core/bodydb.go','core/types/block.go','core/types/wo.go','go.mod','go.sum']
 evidence={'reference_revision':PIN,'driver_sha256':sha(Path(__file__)),'oracle_sources':{n:sha(ROOT/n) for n in names},'sources':{n:sha(go/n) for n in sources},'scope':value['scope'],'runtime_go_dependency':False,'canonical_authority':False,'cases':3200}
 if a.write:corpus.write_bytes(encoded)
 else:assert corpus.read_bytes()==encoded,'Controller oracle differs'
 evidence['vectors_sha256']=sha(corpus)
 source=corpus.with_name('controllers-source.json')
 if a.write:source.write_text(json.dumps(evidence,indent=2)+'\n')
 else:assert json.loads(source.read_text())==evidence,'Controller source binding differs'
 print(json.dumps({'passed':True,'cases':3200,'canonical_authority':False}))
if __name__=='__main__':main()
