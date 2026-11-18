#!/usr/bin/env python3
"""Explicit offline differential oracle for composed parent conversion/controller verification."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True)
 mode=p.add_mutually_exclusive_group(required=True);mode.add_argument('--write',action='store_true');mode.add_argument('--check',action='store_true');a=p.parse_args()
 go=Path(a.go_quai).resolve();checked_reference(go)
 names=['reference/prime-economics/main.go','reference/prime-economics/core-export.go']
 env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 out=ROOT/'results';out.mkdir(exist_ok=True)
 with tempfile.TemporaryDirectory(prefix='quai-prime-economics-oracle-') as directory:
  d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle'
  overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-prime-economics-vectors/main.go'):str(ROOT/names[0]),str(go/'core/native_prime_economics_vectors.go'):str(ROOT/names[1])}}))
  with (out/'prime-economics-oracle-build.log').open('w') as log:
   subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-prime-economics-vectors'],cwd=go,env=env,check=True,stdout=log,stderr=subprocess.STDOUT)
  result=subprocess.run([str(binary)],cwd=d,env=env,capture_output=True)
  (out/'prime-economics-oracle.stdout').write_bytes(result.stdout);(out/'prime-economics-oracle.stderr').write_bytes(result.stderr)
  result.check_returncode()
  value=json.loads(result.stdout)
 assert value['reference_revision']==PIN
 assert len(value['vectors'])==256 and value['go_field_mutations_rejected']==768 and value['go_missing_history_rejected']==512
 corpus=ROOT/'fixtures/protocol/prime-economics.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
 sources=['core/slice.go','core/exchange_controller.go','consensus/misc/rewards.go','common/big.go','core/headerchain.go','params/protocol_params.go','core/bodydb.go','core/types/block.go','core/types/wo.go','go.mod','go.sum']
 evidence={'reference_revision':PIN,'driver_sha256':sha(Path(__file__)),'oracle_sources':{n:sha(ROOT/n) for n in names},'sources':{n:sha(go/n) for n in sources},'scope':value['scope'],'runtime_go_dependency':False,'canonical_authority':False,'cases':256}
 if a.write:corpus.write_bytes(encoded)
 else:assert corpus.read_bytes()==encoded,'Prime economics oracle differs'
 evidence['vectors_sha256']=sha(corpus)
 source=corpus.with_name('prime-economics-source.json')
 if a.write:source.write_text(json.dumps(evidence,indent=2)+'\n')
 else:assert json.loads(source.read_text())==evidence,'Prime economics source binding differs'
 print(json.dumps({'passed':True,'cases':256,'canonical_authority':False}))
if __name__=='__main__':main()
