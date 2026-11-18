#!/usr/bin/env python3
"""Go oracle for filters and subscriptions: go-quai v0.57.0's production
quai/filters PublicFilterAPI and EventSystem behind its JSON-RPC server, over
HTTP and WebSocket, over a test chain with logs, a reorg and the chain events
that core emits (reference/filters). Explicit and offline; never part of the
Rust runtime.

    scripts/gen_filter_vectors.py --go-quai ../go-quai --write
    scripts/gen_filter_vectors.py --go-quai ../go-quai --check
"""
import argparse, json, os, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

SCENARIOS = ['filters', 'expiry', 'subscriptions', 'limit']
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
ref = ROOT / 'reference/filters'
files = {'cmd/quai-filter-vectors/main.go': ref / 'main.go'}
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
with tempfile.TemporaryDirectory(prefix='quai-filters-') as directory:
    d = Path(directory)
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {str(go / k): str(v) for k, v in files.items()}}))
    subprocess.run(['nice', '-n', '10', 'go', 'build', '-p', '2', '-overlay', str(overlay),
                    '-ldflags', '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-filter-vectors'], cwd=go, env=env, check=True)
    raw = subprocess.check_output(['nice', '-n', '10', str(binary)], cwd=d, env=env)
(ROOT / 'results/rpc-complete').mkdir(parents=True, exist_ok=True)
(ROOT / 'results/rpc-complete/filters-oracle-raw.json').write_bytes(raw)
v = json.loads(raw)
assert v['reference_revision'] == PIN, v['reference_revision']
assert [s['name'] for s in v['scenarios']] == SCENARIOS, [s['name'] for s in v['scenarios']]
path = ROOT / 'fixtures/protocol/filters.json'
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('filter and subscription corpus differs')
names = sorted(str(f.relative_to(go)) for f in (go / 'quai/filters').glob('*.go')
               if not f.name.endswith('_test.go'))
names += ['rpc/' + n + '.go' for n in ['handler', 'json', 'subscription', 'websocket', 'server',
                                        'http', 'service', 'types', 'errors']]
names += ['core/headerchain.go', 'core/slice.go', 'core/bodydb.go', 'core/events.go',
          'core/types/wo.go', 'core/types/log.go', 'core/types/transaction_marshalling.go',
          'core/types/receipt.go', 'core/rawdb/accessors_chain.go', 'quai/backend.go',
          'quai/api_backend.go', 'go.mod', 'go.sum']
steps = sum(len(s['steps']) for s in v['scenarios'])
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(f.relative_to(ROOT)): sha(f) for f in files.values()},
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False, 'scope': v['scope']}
target = ROOT / 'fixtures/protocol/filters-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('filter and subscription source evidence differs')
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'scenarios': len(v['scenarios']), 'steps': steps,
                  'blocks': len(v['chain']['blocks'])}))
