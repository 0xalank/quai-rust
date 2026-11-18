#!/usr/bin/env python3
"""Explicit offline native Go trie oracle for incremental state storage."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);g=p.add_mutually_exclusive_group(required=True);g.add_argument('--write',action='store_true');g.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
source=ROOT/'reference/state-db/main.go'
with tempfile.TemporaryDirectory(prefix='quai-state-db-oracle-')as directory:
 d=Path(directory);overlay=d/'overlay.json';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-state-db-vectors/main.go'):str(source)}}));binary=d/'oracle';env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-state-db-vectors'],cwd=go,env=env,check=True)
 result=json.loads(subprocess.check_output([str(binary)],cwd=d,env=env));assert result['reference_revision']==PIN
 raw=(json.dumps(result,sort_keys=True,indent=2)+'\n').encode();path=ROOT/'fixtures/protocol/state-db.json'
 import hashlib
 meta={'reference_revision':PIN,'vectors_sha256':hashlib.sha256(raw).hexdigest(),'driver_sha256':sha(Path(__file__)),'oracle_sha256':sha(source),'go_sources':{n:sha(go/n)for n in ['trie/trie.go','trie/node.go','trie/encoding.go','trie/hasher.go','trie/stacktrie.go','trie/database.go','crypto/crypto.go','core/rawdb/database.go','go.mod','go.sum']},'scope':result['scope'],'public_network':False,'runtime_go_dependency':False}
 evidence=path.with_name('state-db-source.json')
 if a.write:path.write_bytes(raw);evidence.write_text(json.dumps(meta,indent=2)+'\n')
 else:assert path.read_bytes()==raw and json.loads(evidence.read_text())==meta
 print(json.dumps({'passed':True,'steps':sum(len(c['steps'])for c in result['cases']),'bulk_keys':result['bulk']['count']}))
