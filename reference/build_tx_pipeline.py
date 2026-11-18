import argparse,hashlib,json,os,pathlib,shutil,subprocess
parser=argparse.ArgumentParser();parser.add_argument('--stage',required=True);parser.add_argument('--project',required=True);parser.add_argument('--go-quai',required=True);args=parser.parse_args()
s=pathlib.Path(args.stage).resolve();go=pathlib.Path(args.go_quai).resolve();r=pathlib.Path(args.project).resolve()
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
source=json.loads((s/'fixtures/protocol/transactions-source.json').read_text())
revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=go,text=True).strip()
assert revision==source['reference_revision']
assert not subprocess.check_output(['git','diff','HEAD','--','*.go','go.mod','go.sum'],cwd=go)
for n,h in source['sources'].items():assert sha(go/n)==h,n
p=s/'reference/txbench-go/main.go';v=p.read_text().replace(' "fmt"\n','').replace(';fmt.Fprint(io.Discard,"")','');p.write_text(v)
env={**os.environ,'GOCACHE':'/private/tmp/quai-benchmark-go-cache','GOPROXY':'off','GOTOOLCHAIN':'local','CARGO_HOME':'/private/tmp/quai-benchmark-cargo-home','CARGO_TARGET_DIR':'/private/tmp/quai-benchmark-cargo-target','CARGO_NET_OFFLINE':'true','PYTHONDONTWRITEBYTECODE':'1'}
subprocess.run(['gofmt','-w',str(p)],env=env,check=True)
subprocess.run(['cargo','fmt','--manifest-path',str(s/'reference/txbench-rust/Cargo.toml')],env=env,check=True)
overlay=s/'results/go-overlay.json';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-tx-pipeline-bench/main.go'):str(p)}}))
builds={}
for cgo in ['1','0']:
 name='go-cgo' if cgo=='1' else 'go-pure'
 print('Building',name,flush=True)
 command=['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+revision+' -X main.buildCGO='+cgo,'-o',str(s/'bin'/name),'./cmd/quai-tx-pipeline-bench']
 with (s/'results'/(name+'-build.log')).open('w') as log:
  result=subprocess.run(command,cwd=go,env={**env,'CGO_ENABLED':cgo},stdout=log,stderr=subprocess.STDOUT)
 if result.returncode:
  print((s/'results'/(name+'-build.log')).read_text()[-3500:],flush=True)
  if cgo=='1':raise SystemExit(result.returncode)
  builds[name]={'available':False,'build_log':name+'-build.log'};continue
 builds[name]={'available':True,'sha256':sha(s/'bin'/name),'CGO_ENABLED':cgo}
for instrumented in [False,True]:
 name='rust-alloc' if instrumented else 'rust'
 print('Building',name,flush=True)
 command=['cargo','build','--release','--offline','--manifest-path',str(s/'reference/txbench-rust/Cargo.toml')]
 if instrumented:command+=['--features','alloc-count']
 with (s/'results'/(name+'-build.log')).open('w') as log:
  result=subprocess.run(command,cwd=s,env=env,stdout=log,stderr=subprocess.STDOUT)
 if result.returncode:
  print((s/'results'/(name+'-build.log')).read_text()[-5000:],flush=True);raise SystemExit(result.returncode)
 shutil.copy2('/private/tmp/quai-benchmark-cargo-target/release/quai-tx-pipeline-bench',s/'bin'/name)
 builds[name]={'available':True,'sha256':sha(s/'bin'/name),'allocation_instrumented':instrumented}
metadata={'binaries':builds,'reference_revision':revision,'go':subprocess.check_output(['go','version'],text=True).strip(),'rust':subprocess.check_output(['rustc','--version'],text=True).strip(),'sources':{str(p.relative_to(s)):sha(p) for d in ['crates','reference'] for p in sorted((s/d).rglob('*')) if p.is_file() and p.suffix in ['.rs','.go','.toml','.lock']},'corpus_sha256':sha(s/'fixtures/protocol/transactions.json'),'root_source':str(r),'scope':'Offline in-process transaction/signature pipelines; no RPC, node, UTXO ownership, state execution, block validation or sync.'}
(s/'results/build.json').write_text(json.dumps(metadata,indent=2)+'\n')
print(json.dumps({'binaries':builds,'directory':str(s)},indent=2))
