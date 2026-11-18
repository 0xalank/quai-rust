#!/usr/bin/env python3
"""Explicit offline Polaris fork-boundary oracle."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
sources=['reference/polaris/main.go','reference/polaris/core-export.go','reference/header/core-export.go','reference/history/core-export.go','reference/consensus/core-export.go'];targets=['cmd/quai-polaris-vectors/main.go','core/native_polaris_vectors.go','core/native_header_vectors.go','core/native_history_vectors.go','core/native_projection_vectors.go'];env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
with tempfile.TemporaryDirectory(prefix='quai-polaris-oracle-')as directory:
 d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/t):str(ROOT/s)for t,s in zip(targets,sources)}}));subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-polaris-vectors'],cwd=go,env=env,check=True);v=json.loads(subprocess.check_output([str(binary)],cwd=d,env=env))
assert v['polaris_prime_block']==2444574 and len(v['bindings'])==50 and len(v['identities'])==28 and len(v['inclusions'])==12
path=ROOT/'fixtures/protocol/polaris.json';encoded=(json.dumps(v,indent=2,sort_keys=True)+'\n').encode()
if a.write:path.write_bytes(encoded)
elif path.read_bytes()!=encoded:raise SystemExit('Polaris corpus differs')
e={'reference_revision':PIN,'vectors_sha256':sha(path),'driver_sha256':sha(Path(__file__).resolve()),'oracle_sources':{n:sha(ROOT/n)for n in sources},'sources':{n:sha(go/n)for n in ['core/poem.go','core/headerchain_validation.go','core/types/wo.go','core/state_processor.go','core/state_transition.go','core/state/gen_allocs.go','params/protocol_params.go','params/founder_forfeiture_addresses.json','go.mod','go.sum']},'public_network':False,'runtime_go_dependency':False,'scope':v['scope']};target=ROOT/'fixtures/protocol/polaris-source.json'
if a.write:target.write_text(json.dumps(e,indent=2)+'\n')
elif json.loads(target.read_text())!=e:raise SystemExit('Polaris evidence differs')
print(json.dumps({'passed':True,'bindings':50,'identities':28}))
