#!/usr/bin/env python3
"""Explicit v0.57.0 nested-call oracle; never part of the Rust runtime."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
sources=['reference/stateful/main.go','reference/stateful/trace.go'];env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
with tempfile.TemporaryDirectory(prefix='quai-stateful-oracle-')as directory:
 d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-stateful-vectors'/Path(n).name):str(ROOT/n)for n in sources}}))
 subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-stateful-vectors'],cwd=go,env=env,check=True);raw=subprocess.check_output([str(binary)],cwd=d,env=env);(ROOT/'results/stateful-oracle-raw.log').write_bytes(raw);v=json.loads(raw.splitlines()[-1])
assert v['reference_revision']==PIN and len(v['vectors'])==1578
path=ROOT/'fixtures/protocol/stateful.json';encoded=(json.dumps(v,indent=2,sort_keys=True)+'\n').encode()
if a.write:path.write_bytes(encoded)
elif path.read_bytes()!=encoded:raise SystemExit('Stateful corpus differs')
names=sorted({str(p.relative_to(go))for folder in ['core/vm','core/state','params']for p in (go/folder).glob('*.go')if not p.name.endswith('_test.go')}|{'core/genesis.go','crypto/crypto.go','go.mod','go.sum'})
e={'reference_revision':PIN,'vectors_sha256':sha(path),'driver_sha256':sha(Path(__file__).resolve()),'oracle_sources':{n:sha(ROOT/n)for n in sources},'go_sources':{n:sha(go/n)for n in names},'public_network':False,'runtime_go_dependency':False,'scope':v['scope']};target=ROOT/'fixtures/protocol/stateful-source.json'
if a.write:target.write_text(json.dumps(e,indent=2)+'\n')
elif json.loads(target.read_text())!=e:raise SystemExit('Stateful source evidence differs')
print(json.dumps({'passed':True,'cases':len(v['vectors']),'private_genesis_contexts':len(v['private_genesis'])}))
