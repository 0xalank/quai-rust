#!/usr/bin/env python3
"""Explicit pinned-Go canonical number/hash request and absence encoding oracle."""
import argparse,json,os,subprocess,tempfile,hashlib
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',type=Path,required=True);g=p.add_mutually_exclusive_group(required=True);g.add_argument('--write',action='store_true');g.add_argument('--check',action='store_true');a=p.parse_args();go=a.go_quai.resolve();checked_reference(go);helper=ROOT/'reference/forward-wire/main.go'
with tempfile.TemporaryDirectory(prefix='quai-forward-wire-')as directory:
 d=Path(directory);(d/'overlay.json').write_text(json.dumps({'Replace':{str(go/'cmd/quai-forward-wire/main.go'):str(helper)}}));env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 with(ROOT/'results/forward-wire-oracle-build.log').open('w')as log:subprocess.run(['go','build','-overlay',str(d/'overlay.json'),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(d/'oracle'),'./cmd/quai-forward-wire'],cwd=go,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
 v=json.loads(subprocess.check_output([str(d/'oracle')],cwd=d,env=env))
raw=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode();path=ROOT/'fixtures/protocol/forward-wire.json';meta={'reference_revision':PIN,'vectors_sha256':hashlib.sha256(raw).hexdigest(),'driver_sha256':sha(Path(__file__)),'oracle_source_sha256':sha(helper),'go_sources':{n:sha(go/n)for n in ['p2p/pb/proto_services.go','p2p/pb/quai_messages.pb.go','p2p/pb/quai_messages.proto','common/proto_common.proto','go.mod','go.sum']},'public_network':False,'scope':'Actual pinned Go EncodeQuaiRequest/EncodeQuaiResponse canonical number/hash bytes, including zero height, multi-byte height and typed absence; service semantics and native admission are checked separately.'};source=path.with_name('forward-wire-source.json')
if a.write:path.write_bytes(raw);source.write_text(json.dumps(meta,indent=2)+'\n')
else:assert path.read_bytes()==raw and json.loads(source.read_text())==meta
print(json.dumps({'passed':True,'cases':len(v['vectors'])}))
