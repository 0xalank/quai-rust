#!/usr/bin/env python3
"""Native foundation checks and optional explicit loopback Go wire interoperability."""
import argparse,contextlib,datetime,hashlib,json,os,selectors,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,build_oracle,checked_reference,sha

def run(command,cwd,log):
 result=subprocess.run(command,cwd=cwd,text=True,capture_output=True)
 log.write_text(result.stdout+result.stderr)
 if result.returncode:raise RuntimeError('Failed command '+str(command)+'; see '+str(log))
 return result.stdout

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--interop',action='store_true');p.add_argument('--go-quai');a=p.parse_args()
 if a.interop and not a.go_quai:p.error('--interop requires an explicit --go-quai offline reference')
 stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ');out=ROOT/'results'/('foundation-'+stamp);out.mkdir(parents=True)
 build=json.loads((ROOT/'results/rust-build.json').read_text())
 for source,digest in build['rust_sources'].items():
  if sha(ROOT/source)!=digest:raise SystemExit('Rebuild native source before checking: '+source)
 provenance=json.loads((ROOT/'fixtures/protocol/foundation-source.json').read_text())
 assert provenance['vectors_sha256']==sha(ROOT/'fixtures/protocol/foundation.json')
 assert provenance['transaction_corpus_sha256']==sha(ROOT/'fixtures/protocol/transactions.json')
 run(['cargo','build','--locked','--offline','--release','-p','quai-execution','-p','quai-network','--examples'],ROOT,out/'example-build.log')
 target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve();execution_binary=target/'release/examples/check-execution';wire_binary=target/'release/examples/wire-peer'
 foundation=json.loads(run([str(ROOT/'bin/quai-protocol-check'),str(ROOT/'fixtures/protocol/foundation.json')],ROOT,out/'protocol.log'));assert foundation['passed'];(out/'protocol.json').write_text(json.dumps(foundation,indent=2)+'\n')
 execution=json.loads(run([str(execution_binary),str(ROOT/'fixtures/protocol/foundation.json')],ROOT,out/'execution.log'));assert execution['passed'];(out/'execution.json').write_text(json.dumps(execution,indent=2)+'\n')
 interoperability={'performed':False,'libp2p_transport':False,'public_network':False}
 if a.interop:
  go=Path(a.go_quai).resolve();checked_reference(go)
  for source,digest in provenance['sources'].items():assert sha(go/source)==digest,source
  with tempfile.TemporaryDirectory(prefix='quai-wire-interop-')as d:
   oracle=build_oracle(go,d)
   with (out/'go-server.stderr.log').open('w')as errors:
    process=subprocess.Popen([str(oracle),'--wire-server'],stdout=subprocess.PIPE,stderr=errors,cwd=d,text=True)
    try:
     with selectors.DefaultSelector()as selector:
      selector.register(process.stdout,selectors.EVENT_READ)
      if not selector.select(10):raise RuntimeError('Go loopback listener did not start')
     first=process.stdout.readline()
     if not first:raise RuntimeError('Go loopback listener exited; see '+str(out/'go-server.stderr.log'))
     address=json.loads(first)['listen'];assert address.startswith('127.0.0.1:')
     rust=json.loads(run([str(wire_binary),address],ROOT,out/'rust-peer.log'));assert rust['passed']
     stdout,_=process.communicate(timeout=10);assert process.returncode==0
     go_report=json.loads(stdout);assert go_report['passed'] and go_report['exchanges']==rust['exchanges']==3
     interoperability={'performed':True,'passed':True,'exchanges':3,'oversize_rejected':True,'libp2p_transport':False,'public_network':False,'rust':rust,'go':go_report,'go_binary_sha256':sha(oracle)}
     (out/'go-server.stdout.log').write_text(first+stdout)
    finally:
     if process.poll()is None:process.terminate();process.wait(timeout=10)
 summary={'passed':True,'protocol':foundation,'execution':execution,'wire_interoperability':interoperability,'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(ROOT/'fixtures/protocol/foundation.json'),'source_manifest_sha256':sha(ROOT/'fixtures/protocol/foundation-source.json'),'development_binaries':{str(b.name):sha(b)for b in [execution_binary,wire_binary]},'runtime_go_dependency':False,'full_evm':False,'full_consensus':False,'native_libp2p_sync':False,'scope':'Native schemas/commitments, execution prechecks and in-memory ETX state. Optional raw application-wire loopback test uses Go common stream helpers; no libp2p transport or public network.'}
 (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');(ROOT/'results/latest-foundation.json').write_text(json.dumps({'directory':str(out.relative_to(ROOT)),'passed':True},indent=2)+'\n')
 print(json.dumps({'passed':True,'protocol_checks':foundation['checks'],'execution_checks':execution['checks'],'wire':interoperability,'directory':str(out)},indent=2))
if __name__=='__main__':main()
