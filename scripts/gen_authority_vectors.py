#!/usr/bin/env python3
"""Explicit pinned authority-transition oracle; no runtime Go dependency."""
import argparse,json,os,subprocess,tempfile,hashlib
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go);source=ROOT/'reference/authority/main.go'
with tempfile.TemporaryDirectory(prefix='quai-authority-')as directory:
 d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-authority-vectors/main.go'):str(source)}}));env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'};subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-authority-vectors'],cwd=go,env=env,check=True);output=subprocess.check_output([str(binary)],cwd=d,env=env);(ROOT/'results/authority-oracle-raw.log').write_bytes(output);v=json.loads(output.splitlines()[-1])
assert len(v['vectors'])==21 and v['reference_revision']==PIN
path=ROOT/'fixtures/protocol/authority.json';encoded=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode();target=path.with_name('authority-source.json');e={'reference_revision':PIN,'vectors_sha256':hashlib.sha256(encoded).hexdigest(),'oracle_source_sha256':sha(source),'driver_sha256':sha(Path(__file__).resolve()),'go_sources':{name:sha(go/name)for name in ['core/state_transition.go','core/state/statedb.go','core/types/transaction.go','params/protocol_params.go']},'scope':v['scope'],'public_network':False}
if a.write:path.write_bytes(encoded);target.write_text(json.dumps(e,indent=2)+'\n')
elif path.read_bytes()!=encoded or json.loads(target.read_text())!=e:raise SystemExit('Authority corpus differs')
print(json.dumps({'passed':True,'cases':21}))
