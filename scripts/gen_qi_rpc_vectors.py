#!/usr/bin/env python3
"""Go oracle for the Qi and conversion RPC methods: quai_qiToQuai,
quai_quaiToQi, quai_calculateConversionAmount, quai_estimateFeeForQi,
quai_getOutpointDeltasForAddressesInRange, quai_getLockedBalance,
quai_listRunningChains and quai_getWrappedQiDeposit answered by go-quai
v0.57.0's production handlers (internal/quaiapi PublicBlockChainQuaiAPI) and
JSON-RPC server over oracle-built work objects, a rawdb memory database and
committed state (reference/qi-rpc). Explicit and offline; never part of the
Rust runtime.

    scripts/gen_qi_rpc_vectors.py --go-quai ../go-quai --write
    scripts/gen_qi_rpc_vectors.py --go-quai ../go-quai --check
"""
import argparse, json, os, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

SCENARIOS = 17
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
ref = ROOT / 'reference/qi-rpc'
files = {'cmd/quai-qi-rpc-vectors/main.go': ref / 'main.go'}
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
with tempfile.TemporaryDirectory(prefix='quai-qi-rpc-') as directory:
    d = Path(directory)
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {str(go / k): str(v) for k, v in files.items()}}))
    subprocess.run(['nice', '-n', '10', 'go', 'build', '-p', '2', '-overlay', str(overlay),
                    '-ldflags', '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-qi-rpc-vectors'], cwd=go, env=env, check=True)
    raw = subprocess.check_output(['nice', '-n', '10', str(binary)], cwd=d, env=env)
(ROOT / 'results/rpc-complete').mkdir(parents=True, exist_ok=True)
(ROOT / 'results/rpc-complete/qi-rpc-oracle-raw.json').write_bytes(raw)
v = json.loads(raw)
assert v['reference_revision'] == PIN and len(v['scenarios']) == SCENARIOS, len(v['scenarios'])
path = ROOT / 'fixtures/protocol/qi-rpc.json'
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('Qi RPC corpus differs')
names = ['internal/quaiapi/quai_api.go', 'internal/quaiapi/transaction_args.go',
         'internal/quaiapi/backend.go', 'quai/api_backend.go', 'consensus/misc/rewards.go',
         'core/types/qi_tx.go', 'core/types/utxo.go', 'core/types/wo.go',
         'core/types/external_tx.go', 'core/types/transaction.go', 'core/rawdb/accessors_chain.go',
         'core/core.go', 'core/vm/contracts.go', 'core/state/statedb.go',
         'params/protocol_params.go', 'rpc/handler.go', 'rpc/json.go', 'rpc/server.go',
         'rpc/http.go', 'rpc/types.go', 'common/address.go', 'common/types.go', 'go.mod', 'go.sum']
calls = sum(len(s['calls']) for s in v['scenarios'])
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(f.relative_to(ROOT)): sha(f) for f in files.values()},
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False, 'scope': v['scope']}
target = ROOT / 'fixtures/protocol/qi-rpc-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('Qi RPC source evidence differs')
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'scenarios': len(v['scenarios']), 'blocks': len(v['chain']['blocks']),
                  'calls': calls}))
