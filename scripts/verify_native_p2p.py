#!/usr/bin/env python3
"""Actual private Noise/Yamux/persistent-stream interoperability; no public peers."""
import argparse,json,os,subprocess,tempfile,selectors
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
out=ROOT/'results/native-p2p';out.mkdir(exist_ok=True)
env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
def line(process):
 with selectors.DefaultSelector()as s:
  s.register(process.stdout,selectors.EVENT_READ)
  if not s.select(30):raise RuntimeError('Loopback peer startup timed out')
 return json.loads(process.stdout.readline())
with tempfile.TemporaryDirectory(prefix='quai-native-p2p-')as directory:
 d=Path(directory);binary=d/'go-peer';overlay=d/'overlay.json';source=ROOT/'reference/native-p2p/main.go';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-native-p2p/main.go'):str(source)}}))
 with (out/'go-build.log').open('w')as log:subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-native-p2p'],cwd=go,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
 with (out/'rust-build.log').open('w')as build_log:subprocess.run(['cargo','build','--locked','--offline','-p','quai-network','--features','native-transport','--example','private-peer'],cwd=ROOT,env=env,stdout=build_log,stderr=subprocess.STDOUT,check=True)
 rust=Path(env.get('CARGO_TARGET_DIR',str(ROOT/'target')))/'debug/examples/private-peer'
 with (out/'rust.log').open('w')as rlog,(out/'go.log').open('w')as glog:
  rp=subprocess.Popen([str(rust),'go'],cwd=ROOT,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=rlog,text=True);gp=None
  try:
   ready=line(rp);gp=subprocess.Popen([str(binary),ready['peer_id']],cwd=ROOT,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=glog,text=True);peer=line(gp);assert peer['reference_revision']==PIN;rp.stdin.write(json.dumps(peer)+'\n');rp.stdin.flush();result=line(rp);assert result['passed']and result['go_rust_libp2p_exchanges']==8;assert rp.wait(timeout=30)==0;gp.stdin.write('\n');gp.stdin.flush();assert gp.wait(timeout=30)==0
   result.update({'reference_revision':PIN,'go_oracle_source_sha256':sha(source),'go_oracle_binary_sha256':sha(binary),'rust_peer_sha256':sha(rust),'full_native_sync':False,'scope':'Actual Go/Rust Noise + Yamux + /quai/1.0.0 persistent duplex stream with eight protobuf request/response pairs; enrolled ephemeral loopback identities only.'});(out/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
  finally:
   for process in [rp,gp]:
    if process is not None and process.poll()is None:process.terminate();process.wait(timeout=10)
