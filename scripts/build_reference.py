#!/usr/bin/env python3
"""Prepare a separate Go-backed compatibility lab, never the shipping build."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
from lab import ROOT

parser=argparse.ArgumentParser()
parser.add_argument('--out',default=str(ROOT/'reference-build'))
parser.add_argument('--go-quai',default=str(ROOT.parent/'go-quai'))
args=parser.parse_args()
out=Path(args.out).resolve();go=Path(args.go_quai).resolve()
if out==ROOT or ROOT in out.parents and out.parts[len(ROOT.parts)] in ['crates','scripts','reference','sdk','bin','fixtures','results']:
    raise SystemExit('Reference output must be separate from source/runtime directories')
if out.exists() and any(out.iterdir()):raise SystemExit('Choose a new empty reference output directory')
if not(go/'go.mod').is_file():raise SystemExit('Set --go-quai to the pinned reference checkout')
out.mkdir(parents=True,exist_ok=True)
for name in ['crates','scripts','reference','configs','fixtures','compatibility','sdk']:
    shutil.copytree(ROOT/name,out/name,ignore=shutil.ignore_patterns('node_modules','__pycache__'))
for name in ['Cargo.toml','Cargo.lock','AGENTS.md']:
    shutil.copy2(ROOT/name,out/name)
if (ROOT/'sdk/node_modules').is_dir():
    (out/'sdk/node_modules').symlink_to(ROOT/'sdk/node_modules',target_is_directory=True)
env={**os.environ,'QUAI_ISOLATED_REFERENCE':'1','GO_QUAI_DIR':str(go)}
subprocess.run([sys.executable,'-B',str(out/'scripts/build_reference_inner.py'),'--go-quai',str(go)],cwd=out,env=env,check=True)
print('Reference-only Go binaries and gateway:',out/'bin')
print('Run compatibility scripts from:',out)
