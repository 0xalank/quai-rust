#!/usr/bin/env python3
import argparse
import json
import os
from lab import ROOT,RUST
p=argparse.ArgumentParser()
p.add_argument('--config',default='configs/baseline.json')
p.add_argument('--mode',choices=['fixture','gateway','hybrid','snapshot'],default='fixture')
p.add_argument('--listen',default='127.0.0.1:8547')
p.add_argument('--upstream')
p.add_argument('--snapshot')
p.add_argument('--native-canonical-reads',action='store_true')
p.add_argument('--follow',action='store_true')
p.add_argument('--poll-ms',type=int,default=1000)
p.add_argument('--window-blocks',type=int,default=128)
a=p.parse_args()
config=json.loads((ROOT/a.config).read_text())
if set(config)-{'workers','batch_concurrency','encoding'}:raise SystemExit('Unexpected tuning configuration keys')
cmd=[str(RUST),'--listen',a.listen,'--mode',a.mode]
for key,value in config.items():cmd+=['--'+key.replace('_','-'),str(value)]
if a.snapshot:cmd+=['--snapshot',a.snapshot]
if a.native_canonical_reads:cmd+=['--native-canonical-reads','true']
if a.follow:cmd+=['--follow','true','--poll-ms',str(a.poll_ms),'--window-blocks',str(a.window_blocks)]
if a.upstream:cmd+=['--upstream',a.upstream]
os.execv(str(RUST),cmd)
