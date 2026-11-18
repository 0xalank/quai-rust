#!/usr/bin/env python3
"""Explicit development schema regeneration; never called by Cargo/default build."""
import argparse,hashlib,json,subprocess,tempfile
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
from reference_baseline import REVISION as PIN
SOURCES=['common/proto_common.proto','core/types/proto_block.proto','core/rawdb/db.proto','p2p/pb/quai_messages.proto','p2p/node/peerManager/peerdb/peer_info.proto']
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True);mode=p.add_mutually_exclusive_group(required=True);mode.add_argument('--write',action='store_true');mode.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve()
 revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=go,text=True).strip()
 if revision!=PIN:raise SystemExit('Review a separate schema baseline for a different revision')
 if subprocess.check_output(['git','diff','HEAD','--',*SOURCES],cwd=go):raise SystemExit('Pinned schema sources are modified')
 destination=ROOT/'crates/quai-primitives/src/schema';helper=ROOT/'reference/prost-schema'
 with tempfile.TemporaryDirectory(prefix='quai-schema-')as d:
  output=Path(d)
  subprocess.run(['cargo','run','--locked','--offline','--manifest-path',str(helper/'Cargo.toml'),'--',str(go),str(output)],check=True)
  files=sorted(output.glob('*.rs'));subprocess.run(['rustfmt','--edition','2021',*[str(f) for f in files]],check=True)
  for f in files:
   if a.write:(destination/f.name).write_bytes(f.read_bytes())
   elif (destination/f.name).read_bytes()!=f.read_bytes():raise SystemExit('Schema drift: '+f.name)
 metadata={'schema':1,'reference_revision':revision,'sources':{name:sha(go/name)for name in SOURCES},'generator':{str(f.relative_to(ROOT)):sha(f)for f in [Path(__file__).resolve(),helper/'Cargo.toml',helper/'Cargo.lock',helper/'src/main.rs']},'protoc':subprocess.check_output(['protoc','--version'],text=True).strip(),'generated':{str(f.relative_to(ROOT)):sha(f)for f in sorted(destination.glob('*.rs'))if f.name!='mod.rs'},'scope':'All five pinned schemas; raw field bindings and bounded preflight descriptors, not complete semantic protocol validation. Go/protoc are absent from the default build.'}
 path=ROOT/'fixtures/protocol/schema-source.json'
 if a.write:path.write_text(json.dumps(metadata,indent=2)+'\n')
 elif json.loads(path.read_text())!=metadata:raise SystemExit('Schema source metadata drift')
 print(json.dumps({'passed':True,'mode':'write'if a.write else'check','schema_files':len(SOURCES),'generated_modules':len(metadata['generated'])}))
if __name__=='__main__':main()
