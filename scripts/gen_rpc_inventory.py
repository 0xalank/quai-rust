#!/usr/bin/env python3
"""Go oracle for the RPC inventory: every JSON-RPC method and subscription
that go-quai v0.57.0 registers for a zone node, with the Go function and
file:line that serves it, and whether a node with the default flags serves it
over HTTP and over WebSocket (reference/rpc-inventory: the rpc.API literals of
quaiapi.GetAPIs and Quai.APIs, makeConfigNode's default configuration, the
production node.Node and rpc.Server, probes that never run a handler).
Explicit and offline; never part of the Rust runtime.

    scripts/gen_rpc_inventory.py --go-quai ../go-quai --write
    scripts/gen_rpc_inventory.py --go-quai ../go-quai --check

scripts/render_rpc_parity.py turns the inventory into docs/rpc-parity.md.
"""
import argparse, json, os, shutil, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
ref = ROOT / 'reference/rpc-inventory'
files = {'cmd/quai-rpc-inventory/main.go': ref / 'main.go',
         'rpc/rpc_inventory_export.go': ref / 'rpc-export.go',
         'cmd/utils/rpc_inventory_export.go': ref / 'utils-export.go'}
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
(ROOT / 'target').mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='quai-rpc-inventory-', dir=ROOT / 'target') as directory:
    d = Path(directory)
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {str(go / k): str(v) for k, v in files.items()}}))
    subprocess.run(['nice', '-n', '19', 'go', 'build', '-p', '2', '-overlay', str(overlay),
                    '-ldflags', '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-rpc-inventory'], cwd=go, env=env, check=True)
    # params.Version reads VERSION from the working directory, as the node
    # does when it runs from its checkout.
    shutil.copy(go / 'VERSION', d / 'VERSION')
    raw = subprocess.check_output(['nice', '-n', '19', str(binary), str(go)], cwd=d, env=env)
(ROOT / 'results/rpc-parity').mkdir(parents=True, exist_ok=True)
(ROOT / 'results/rpc-parity/rpc-inventory-raw.json').write_bytes(raw)
v = json.loads(raw)
assert v['reference_revision'] == PIN, v['reference_revision']
path = ROOT / 'fixtures/protocol/rpc-inventory.json'
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('RPC inventory differs')
names = ['internal/quaiapi/backend.go', 'internal/quaiapi/api.go', 'internal/quaiapi/quai_api.go',
         'quai/backend.go', 'quai/api.go', 'quai/filters/api.go', 'cmd/utils/cmd.go',
         'cmd/utils/flags.go', 'node/node.go', 'node/rpcstack.go', 'node/defaults.go',
         'rpc/server.go', 'rpc/service.go', 'rpc/handler.go', 'rpc/json.go', 'rpc/errors.go',
         'go.mod', 'go.sum']
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(f.relative_to(ROOT)): sha(f) for f in files.values()},
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False, 'scope': v['scope']}
target = ROOT / 'fixtures/protocol/rpc-inventory-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('RPC inventory source evidence differs')
methods = v['methods']
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'apis': len(v['apis']), 'methods': len(methods),
                  'http_default': sum(x['http']['served'] for x in methods),
                  'ws_default': sum(x['ws']['served'] for x in methods),
                  'outside': len(v['outside'])}))
