#!/usr/bin/env python3
"""Explicit offline oracle step; never invoked by scripts/build.py or cargo."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
from lab import ROOT, save, sha
parser=argparse.ArgumentParser();parser.add_argument('--go-quai',default=os.environ.get('GO_QUAI_DIR',ROOT.parent/'go-quai'));args=parser.parse_args()
go=Path(args.go_quai).resolve();revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=go,text=True).strip()
if revision!='33db82f2891b267ba898ca38bd2dd8bf75cbb072':raise SystemExit('Reference revision differs; establish a new versioned baseline first')
if subprocess.check_output(['git','diff','HEAD','--','*.go','go.mod','go.sum'],cwd=go):raise SystemExit('Reference has tracked source changes')
subprocess.run(['gofmt','-w',str(ROOT/'reference/protocol/main.go')],check=True)
with tempfile.TemporaryDirectory(prefix='quai-protocol-oracle-') as directory:
    overlay=Path(directory)/'overlay.json';binary=Path(directory)/'protocol-vectors'
    overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-protocol-vectors/main.go'):str(ROOT/'reference/protocol/main.go')}}))
    subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+revision,'-o',str(binary),'./cmd/quai-protocol-vectors'],cwd=go,check=True)
    value=json.loads(subprocess.check_output([str(binary)],cwd=go))
    save(ROOT/'fixtures/protocol/work-headers.json',value)
    save(ROOT/'fixtures/protocol/source.json',{'reference_revision':revision,'generator_sha256':sha(ROOT/'reference/protocol/main.go'),'vectors_sha256':sha(ROOT/'fixtures/protocol/work-headers.json'),'sources':{n:sha(go/n) for n in ['core/types/wo.go','core/types/proto_block.proto','common/proto_common.proto','params/protocol_params.go']},'scope':value['scope']})
print('Generated pinned offline protocol vectors; no runtime service was started.')
