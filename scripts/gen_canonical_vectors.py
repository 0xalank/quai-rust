#!/usr/bin/env python3
"""Explicit Go coordinator and canonical topology oracle over frozen native blocks."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);a=p.add_mutually_exclusive_group(required=True);a.add_argument('--write',action='store_true');a.add_argument('--check',action='store_true');args=p.parse_args();go=Path(args.go_quai).resolve();checked_reference(go)
source=ROOT/'fixtures/protocol/weakshares.json';evidence=source.with_name('weakshares-source.json');m=json.loads(evidence.read_text());assert m['vectors_sha256']==sha(source)and m['reference_revision']==PIN
with tempfile.TemporaryDirectory(prefix='quai-canonical-oracle-')as directory:
 d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-canonical-vectors/main.go'):str(ROOT/'reference/canonical/main.go'),str(go/'cmd/utils/native_canonical.go'):str(ROOT/'reference/canonical/coordinator.go'),str(go/'core/native_canonical_topology.go'):str(ROOT/'reference/canonical/topology.go')}}));env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'};subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-canonical-vectors'],cwd=go,env=env,check=True);v=json.loads(subprocess.check_output([str(binary),str(source)],cwd=d,env=env))
assert len(v['canonical_events'])==25 and v['reference_revision']==PIN
path=ROOT/'fixtures/protocol/canonical.json';raw=(json.dumps(v,sort_keys=True,indent=2)+'\n').encode()
meta={'reference_revision':PIN,'vectors_sha256':__import__('hashlib').sha256(raw).hexdigest(),'input_corpus_sha256':sha(source),'input_evidence_sha256':sha(evidence),'profile_sha256':sha(ROOT/'configs/private-hierarchy-v1.json'),'driver_sha256':sha(Path(__file__).resolve()),'oracle_sources':{str(n.relative_to(ROOT)):sha(n)for n in sorted((ROOT/'reference/canonical').glob('*.go'))},'go_sources':{n:sha(go/n)for n in ['cmd/utils/hierarchical_coordinator.go','core/headerchain.go','core/bodydb.go','core/rawdb/accessors_chain.go','core/poem.go','core/types/wo.go','go.mod','go.sum']},'public_network':False,'runtime_go_dependency':False}
target=path.with_name('canonical-source.json')
if args.write:path.write_bytes(raw);target.write_text(json.dumps(meta,indent=2)+'\n')
else:assert path.read_bytes()==raw and json.loads(target.read_text())==meta
print(json.dumps({'passed':True,'events':len(v['canonical_events']),'first_branch':v['first_branch'],'winning_branch':v['winning_branch']}))
