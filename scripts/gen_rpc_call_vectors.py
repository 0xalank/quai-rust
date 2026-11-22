#!/usr/bin/env python3
"""Go oracle for the native RPC calls: quai_call, quai_estimateGas,
quai_createAccessList and quai_gasPrice, and their eth_ twins (scenario
"eth": PublicBlockChainAPI's Call, EstimateGas and CreateAccessList and the
shared GasPrice), answered by go-quai v0.57.0's production handlers
(internal/quaiapi) and JSON-RPC server over controlled state and headers
(reference/rpc-calls). Explicit and offline; never part of the Rust
runtime.

    scripts/gen_rpc_call_vectors.py --go-quai ../go-quai --write
    scripts/gen_rpc_call_vectors.py --go-quai ../go-quai --check
"""
import argparse, json, os, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

SCENARIOS = 14
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
main = ROOT / 'reference/rpc-calls/main.go'
adapter = ROOT / 'reference/rpc-calls/core-export.go'
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
with tempfile.TemporaryDirectory(prefix='quai-rpc-calls-') as directory:
    d = Path(directory)
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {
        str(go / 'cmd/quai-rpc-call-vectors/main.go'): str(main),
        str(go / 'core/native_rpc_calls_vectors.go'): str(adapter)}}))
    subprocess.run(['nice', '-n', '19', 'go', 'build', '-p', '2', '-overlay', str(overlay), '-ldflags',
                    '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-rpc-call-vectors'], cwd=go, env=env, check=True)
    raw = subprocess.check_output([str(binary)], cwd=d, env=env)
(ROOT / 'results/rpc-calls').mkdir(parents=True, exist_ok=True)
(ROOT / 'results/rpc-calls/oracle-raw.json').write_bytes(raw)
v = json.loads(raw)
assert v['reference_revision'] == PIN and len(v['scenarios']) == SCENARIOS
path = ROOT / 'fixtures/protocol/rpc-calls.json'
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('RPC call corpus differs')
names = ['internal/quaiapi/api.go', 'internal/quaiapi/quai_api.go',
         'internal/quaiapi/transaction_args.go', 'internal/quaiapi/backend.go',
         'core/state_transition.go', 'core/evm.go', 'core/headerchain.go',
         'core/state/statedb.go', 'core/state/access_list.go',
         'core/vm/access_list_tracer.go', 'core/vm/interpreter.go', 'core/vm/evm.go',
         'core/vm/instructions.go', 'core/vm/operations_acl.go', 'core/vm/errors.go',
         'core/vm/opcodes.go', 'core/vm/contracts.go', 'consensus/misc/rewards.go',
         'params/protocol_params.go', 'rpc/server.go', 'rpc/handler.go', 'rpc/http.go',
         'common/address.go', 'common/internal_address.go', 'go.mod', 'go.sum']
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(n.relative_to(ROOT)): sha(n) for n in (main, adapter)},
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False, 'scope': v['scope']}
target = ROOT / 'fixtures/protocol/rpc-calls-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('RPC call source evidence differs')
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'scenarios': len(v['scenarios']),
                  'vectors': sum(len(s['vectors']) for s in v['scenarios'])}))
