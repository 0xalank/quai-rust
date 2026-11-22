#!/usr/bin/env python3
"""Go oracle for the read methods that a default go-quai v0.57.0 zone node
serves and the native Rust RPC ported in the rpc-parity step: the `quai_`
block, fee, quote, supply, lockup, proof and identity methods, `net_` peer
counts and the `eth_` namespace, answered by the production quaiapi
handlers behind rpc.Server over a test chain with receipts, a side branch,
one committed state, a prime terminus, UTXO set sizes, the address UTXO
index and coinbase lockup records (reference/rpc-parity). Explicit and
offline; never part of the Rust runtime.

    scripts/gen_rpc_parity_vectors.py --go-quai ../go-quai --write
    scripts/gen_rpc_parity_vectors.py --go-quai ../go-quai --check
"""
import argparse, json, os, shutil, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

SCENARIOS = ['head-b5', 'head-c5']
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
ref = ROOT / 'reference/rpc-parity'
files = {'cmd/quai-rpc-parity-vectors/main.go': ref / 'main.go'}
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
(ROOT / 'target').mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='quai-rpc-parity-', dir=ROOT / 'target') as directory:
    d = Path(directory)
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {str(go / k): str(v) for k, v in files.items()}}))
    subprocess.run(['nice', '-n', '19', 'go', 'build', '-p', '2', '-overlay', str(overlay),
                    '-ldflags', '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-rpc-parity-vectors'], cwd=go, env=env, check=True)
    # params.Version reads VERSION from the working directory.
    shutil.copy(go / 'VERSION', d / 'VERSION')
    raw = subprocess.check_output(['nice', '-n', '19', str(binary), str(go)], cwd=d, env=env)
(ROOT / 'results/rpc-parity').mkdir(parents=True, exist_ok=True)
(ROOT / 'results/rpc-parity/rpc-parity-oracle-raw.json').write_bytes(raw)
v = json.loads(raw)
assert v['reference_revision'] == PIN, v['reference_revision']
assert [s['name'] for s in v['scenarios']] == SCENARIOS, [s['name'] for s in v['scenarios']]
path = ROOT / 'fixtures/protocol/rpc-parity.json'
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('RPC parity corpus differs')
names = ['internal/quaiapi/quai_api.go', 'internal/quaiapi/api.go', 'internal/quaiapi/backend.go',
         'internal/quaiapi/transaction_args.go', 'quai/api_backend.go', 'core/core.go',
         'core/slice.go', 'core/state_processor.go', 'core/headerchain.go',
         'core/rawdb/accessors_chain.go', 'core/rawdb/accessors_indexes.go', 'core/rawdb/schema.go',
         'core/state/statedb.go', 'core/state/state_object.go', 'core/types/wo.go',
         'core/types/block.go', 'core/types/receipt.go', 'core/types/transaction.go',
         'core/types/bloom9.go', 'core/vm/contracts.go', 'consensus/misc/rewards.go',
         'params/protocol_params.go', 'params/genesis_alloc.go', 'params/genesis_alloc.json',
         'params/forfeiture_addresses.json', 'params/founder_forfeiture_addresses.json',
         'params/version.go', 'trie/proof.go', 'rpc/handler.go', 'rpc/json.go', 'rpc/server.go',
         'rpc/types.go', 'common/address.go', 'common/types.go', 'common/hexutil/json.go',
         'VERSION', 'go.mod', 'go.sum']
calls = sum(len(s['calls']) for s in v['scenarios'])
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(f.relative_to(ROOT)): sha(f) for f in files.values()},
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False, 'scope': v['scope']}
target = ROOT / 'fixtures/protocol/rpc-parity-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('RPC parity source evidence differs')
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'scenarios': len(v['scenarios']), 'blocks': len(v['chain']['blocks']),
                  'calls': calls}))
