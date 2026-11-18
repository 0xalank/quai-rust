#!/usr/bin/env python3
"""Go oracle for the transaction pool: go-quai v0.57.0's production
core.TxPool and quaiapi pool handlers behind its JSON-RPC server, over a test
chain, driven step by step (reference/txpool). Explicit and offline; never
part of the Rust runtime.

    scripts/gen_txpool_vectors.py --go-quai ../go-quai --write
    scripts/gen_txpool_vectors.py --go-quai ../go-quai --check
"""
import argparse, json, os, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

SCENARIOS = 26
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
ref = ROOT / 'reference/txpool'
files = {
    'cmd/quai-txpool-vectors/main.go': ref / 'main.go',
    'cmd/quai-txpool-vectors/scenarios.go': ref / 'scenarios.go',
    'core/txpool_oracle_export.go': ref / 'core-export.go',
    'core/types/txpool_oracle_export.go': ref / 'types-export.go',
}
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
with tempfile.TemporaryDirectory(prefix='quai-txpool-') as directory:
    d = Path(directory)
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {str(go / k): str(v) for k, v in files.items()}}))
    subprocess.run(['go', 'build', '-overlay', str(overlay), '-ldflags',
                    '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-txpool-vectors'], cwd=go, env=env, check=True)
    raw = subprocess.check_output([str(binary)], cwd=d, env=env)
(ROOT / 'results/txpool').mkdir(parents=True, exist_ok=True)
(ROOT / 'results/txpool/oracle-raw.json').write_bytes(raw)
v = json.loads(raw)
assert v['reference_revision'] == PIN and len(v['scenarios']) == SCENARIOS, len(v['scenarios'])
path = ROOT / 'fixtures/protocol/txpool.json'
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('transaction pool corpus differs')
names = ['core/tx_pool.go', 'core/tx_list.go', 'core/tx_noncer.go', 'core/tx_cacher.go',
         'core/state_processor.go', 'core/state_transition.go', 'core/error.go',
         'core/types/transaction.go', 'core/types/transaction_signing.go',
         'core/types/quai_tx.go', 'core/types/qi_tx.go', 'core/types/external_tx.go',
         'core/types/utxo.go', 'core/rawdb/accessors_chain.go', 'consensus/misc/rewards.go',
         'internal/quaiapi/api.go', 'internal/quaiapi/backend.go', 'quai/api_backend.go',
         'core/core.go', 'quai/p2p_backend.go', 'quaiclient/quaiclient.go',
         'rpc/client.go', 'rpc/http.go', 'rpc/json.go', 'rpc/server.go', 'rpc/handler.go',
         'common/address.go', 'common/internal_address.go', 'common/types.go',
         'crypto/crypto.go', 'crypto/signature_cgo.go', 'params/protocol_params.go',
         'cmd/utils/flags.go', 'quai/quaiconfig/config.go', 'go.mod', 'go.sum']
steps = sum(len(s['steps']) for s in v['scenarios'])
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(f.relative_to(ROOT)): sha(f) for f in files.values()},
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False, 'scope': v['scope']}
target = ROOT / 'fixtures/protocol/txpool-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('transaction pool source evidence differs')
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'scenarios': len(v['scenarios']), 'steps': steps}))
