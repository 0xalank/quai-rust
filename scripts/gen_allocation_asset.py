#!/usr/bin/env python3
"""Explicit source import. Native builds consume the committed binary index."""
import argparse,hashlib,json,struct
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
names=['params/genesis_alloc.json','params/forfeiture_addresses.json','params/founder_forfeiture_addresses.json'];raw=[json.loads((go/n).read_text())for n in names];sets=[{bytes.fromhex(v[2:])for v in rows}for rows in raw[1:]];rows=[]
for account in raw[0]:
 address=bytes.fromhex(account['address'][2:]);flags=int(address in sets[0])|int(address in sets[1])<<1
 for height,value in account['balanceSchedule'].items():
  assert 0<=int(value)<1<<256 and int(height)>=0
  rows.append((int(height),address,flags,int(value)))
rows.sort();encoded=b'QUAI-ALLOCS-1\n'+struct.pack('>I',len(rows))+b''.join(struct.pack('>Q',h)+addr+bytes([flags])+v.to_bytes(32,'big')for h,addr,flags,v in rows)
path=ROOT/'crates/quai-execution/assets/allocations.bin';manifest=path.with_suffix('.json');e={'reference_revision':PIN,'source_sha256':{n:sha(go/n)for n in names},'asset_sha256':hashlib.sha256(encoded).hexdigest(),'accounts':len(raw[0]),'entries':len(rows),'format':'14-byte magic, u32be count, entries (u64be height, 20-byte address, u8 forfeiture flags, u256be amount), sorted by height and address','license':'GPL-3.0-only; official go-quai parameter data','driver_sha256':sha(Path(__file__).resolve())}
if a.write:path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(encoded);manifest.write_text(json.dumps(e,indent=2)+'\n')
elif path.read_bytes()!=encoded or json.loads(manifest.read_text())!=e:raise SystemExit('Allocation asset differs')
print(json.dumps({'passed':True,'accounts':len(raw[0]),'entries':len(rows),'bytes':len(encoded)}))
