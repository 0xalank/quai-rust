#!/usr/bin/env python3
import argparse
import datetime
import os
from pathlib import Path
import platform
import shutil
import subprocess
from lab import ROOT,save,sha
from build_chain import build as build_chain
from build_full import build as build_full
from build_live import build as build_live

if os.environ.get('QUAI_ISOLATED_REFERENCE') != '1':
    raise SystemExit('Use scripts/build_reference.py; this helper belongs in its isolated output directory')
p=argparse.ArgumentParser()
p.add_argument('--go-quai',default=os.environ.get('GO_QUAI_DIR',str(ROOT.parent/'go-quai')))
args=p.parse_args()
go_quai=Path(args.go_quai).resolve()
if not (go_quai/'go.mod').is_file():raise SystemExit('Set --go-quai to the go-quai checkout')
env=dict(os.environ)
target=Path(env.get('CARGO_TARGET_DIR',str(ROOT/'target'))).resolve()
env['CARGO_TARGET_DIR']=str(target)
for command in [['cargo','fmt','--check'],['cargo','test','--locked','--features','quai-rpc/reference-gateway'],['cargo','build','--release','--locked','--features','quai-rpc/reference-gateway']]:
    subprocess.run(command,cwd=ROOT,env=env,check=True)
(ROOT/'bin').mkdir(exist_ok=True)
shutil.copy2(target/'release/quai-rpc',ROOT/'bin/quai-rpc')
subprocess.run(['go','build','-o',str(ROOT/'bin/quai-reference'),str(ROOT/'reference/go/main.go')],cwd=go_quai,env=env,check=True)
build_chain(go_quai)
build_full(go_quai)
build_live(go_quai)
subprocess.run(['gofmt','-w',str(ROOT/'reference/load/main.go')],check=True)
subprocess.run(['go','build','-o',str(ROOT/'bin/quai-read-load'),str(ROOT/'reference/load/main.go')],cwd=go_quai,env=env,check=True)
subprocess.run(['gofmt','-w',str(ROOT/'reference/freshload/main.go')],check=True)
subprocess.run(['go','build','-o',str(ROOT/'bin/quai-fresh-load'),str(ROOT/'reference/freshload/main.go')],cwd=go_quai,env=env,check=True)
metadata={
    'built_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'platform':platform.platform(),
    'go_quai_revision':subprocess.check_output(['git','rev-parse','HEAD'],cwd=go_quai,text=True).strip(),
    'rustc':subprocess.check_output(['rustc','--version'],text=True).strip(),
    'go':subprocess.check_output(['go','version'],text=True).strip(),
    'cargo_lock_sha256':sha(ROOT/'Cargo.lock'),
    'binaries':{name:sha(ROOT/'bin'/name) for name in ['quai-rpc','quai-reference','quai-chain-oracle','go-quai','quai-scenario-oracle','quai-read-load','go-quai-local-fixture','quai-local-mine','quai-fresh-load']},
    'reference_sources':{str(f.relative_to(ROOT)):sha(f) for f in sorted((ROOT/'reference').rglob('*.go'))},
    'rust_sources':{str(f.relative_to(ROOT)):sha(f) for f in sorted((ROOT/'crates').rglob('*.rs'))},
}
save(ROOT/'results/build.json',metadata)
print('Built Rust RPC, fixture reference and controlled-state oracle')
