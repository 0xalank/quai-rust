#!/usr/bin/env python3
"""Explicit offline Go oracle for schemas, commitments and bounded execution state."""
import argparse,hashlib,json,os,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
from reference_baseline import REVISION as PIN
SOURCES=['common/proto_common.proto','common/types.go','common/stream_services.go','core/types/proto_block.proto','core/types/block.go','core/types/wo.go','core/types/hashing.go','core/types/receipt.go','core/types/bloom9.go','core/types/log.go','core/types/transaction.go','core/types/qi_tx.go','core/types/utxo.go','core/state/statedb.go','core/state/database.go','core/state_processor.go','core/state_transition.go','trie/trie.go','trie/stacktrie.go','trie/secure_trie.go','trie/hasher.go','params/protocol_params.go','p2p/protocol/constants.go','p2p/node/pubsubManager/utils.go','p2p/pb/quai_messages.proto','core/rawdb/db.proto','p2p/node/peerManager/peerdb/peer_info.proto','go.mod','go.sum']
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def checked_reference(go):
 if subprocess.check_output(['git','rev-parse','HEAD'],cwd=go,text=True).strip()!=PIN:raise SystemExit('Different oracle revision; review a new baseline')
 if subprocess.check_output(['git','diff','HEAD','--','*.go','*.proto','go.mod','go.sum'],cwd=go):raise SystemExit('Pinned tracked oracle sources are modified')
def build_oracle(go,directory):
 checked_reference(go);source=ROOT/'reference/foundation/main.go';directory=Path(directory);overlay=directory/'overlay.json';binary=directory/'foundation-oracle'
 overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-foundation-vectors/main.go'):str(source)}}))
 env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-foundation-vectors'],cwd=go,env=env,check=True)
 return binary

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True);mode=p.add_mutually_exclusive_group(required=True);mode.add_argument('--write',action='store_true');mode.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve()
 with tempfile.TemporaryDirectory(prefix='quai-foundation-oracle-')as d:
  binary=build_oracle(go,d)
  value=json.loads(subprocess.check_output([str(binary),str(ROOT/'fixtures/protocol/transactions.json')],cwd=d))
 encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode();path=ROOT/'fixtures/protocol/foundation.json'
 if a.write:path.write_bytes(encoded)
 elif path.read_bytes()!=encoded:raise SystemExit('Foundation corpus drifted from pinned oracle')
 source=ROOT/'reference/foundation/main.go'
 metadata={'schema':1,'reference_revision':PIN,'generator_sha256':sha(source),'driver_sha256':sha(Path(__file__).resolve()),'vectors_sha256':sha(path),'transaction_corpus_sha256':sha(ROOT/'fixtures/protocol/transactions.json'),'sources':{n:sha(go/n)for n in SOURCES},'scope':value['scope'],'execution_scope':value['execution']['scope'],'keys':'Public deterministic test keys only; no production wallets or public network activity.'}
 target=ROOT/'fixtures/protocol/foundation-source.json'
 if a.write:target.write_text(json.dumps(metadata,indent=2)+'\n')
 elif json.loads(target.read_text())!=metadata:raise SystemExit('Foundation source evidence drift')
 print(json.dumps({'passed':True,'mode':'write'if a.write else'check','schema_vectors':len(value['schema_vectors']),'work_objects':len(value['work_objects']),'etx_steps':sum(len(t['steps'])for t in value['execution']['etx_traces'])}))
if __name__=='__main__':main()
