#!/usr/bin/env python3
"""Explicitly record the reviewed benchmark contract before optimization."""
from lab import ROOT,save,sha
names=['fixtures/workloads.json','reference/go/main.go','scripts/lab.py','scripts/verify.py','scripts/tune.py','configs/baseline.json']
save(ROOT/'fixtures/contract-sha256.json',{name:sha(ROOT/name) for name in names})
print('Benchmark contract recorded; do not reseal during an optimization trial.')
