#!/usr/bin/env python3
"""Go oracle for precompile addresses, the access list of a creation,
address-scope errors in nested frames, lockup-contract calls and precompile
inputs (reference/precompiles). Signed transactions run through go-quai
v0.57.0's actual applyTransaction, observed as the settlement oracle does
(reference/settlement/core-export.go); precompile inputs run through
vm.RunPrecompiledContract. Explicit and offline; never part of the Rust
runtime.

    scripts/gen_precompile_vectors.py --go-quai ../go-quai --write
    scripts/gen_precompile_vectors.py --go-quai ../go-quai --check
"""
import argparse, json, os, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

GROUPS = {'creation': 98, 'translation': 531, 'balance': 18, 'scope': 180, 'lockup': 88}
RUNS = 668
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
main = ROOT / 'reference/precompiles/main.go'
adapter = ROOT / 'reference/settlement/core-export.go'
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
with tempfile.TemporaryDirectory(prefix='quai-precompiles-') as directory:
    d = Path(directory)
    original = (go / 'core/state_processor.go').read_text()
    needle = 'result, err := ApplyMessage(evm, msg, gp)'
    assert original.count(needle) == 1
    instrumented = d / 'state_processor.go'
    instrumented.write_text(original.replace(needle, needle + '\n NativeSettlementLast = result'))
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {
        str(go / 'cmd/quai-precompile-vectors/main.go'): str(main),
        str(go / 'core/native_settlement_vectors.go'): str(adapter),
        str(go / 'core/state_processor.go'): str(instrumented)}}))
    subprocess.run(['go', 'build', '-overlay', str(overlay), '-ldflags',
                    '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-precompile-vectors'], cwd=go, env=env, check=True)
    raw = subprocess.check_output([str(binary)], cwd=d, env=env)
(ROOT / 'results/exec-findings').mkdir(parents=True, exist_ok=True)
(ROOT / 'results/exec-findings/precompile-oracle-raw.json').write_bytes(raw)
v = json.loads(raw)
assert v['reference_revision'] == PIN
assert v['groups'] == GROUPS, v['groups']
assert len(v['runs']) == RUNS, len(v['runs'])
assert len(v['vectors']) == sum(GROUPS.values())
path = ROOT / 'fixtures/protocol/precompiles.json'
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('Precompile corpus differs')
names = ['core/state_processor.go', 'core/state_transition.go', 'core/evm.go',
         'core/headerchain.go', 'core/state/statedb.go', 'core/state/access_list.go',
         'core/state/state_object.go', 'core/vm/evm.go', 'core/vm/contracts.go',
         'core/vm/instructions.go', 'core/vm/interpreter.go', 'core/vm/gas_table.go',
         'core/vm/operations_acl.go', 'core/vm/errors.go', 'core/types/receipt.go',
         'core/types/transaction.go', 'crypto/crypto.go', 'crypto/bn256/bn256_fast.go',
         'crypto/blake2b/blake2b.go', 'common/address.go', 'common/types.go',
         'params/protocol_params.go', 'go.mod', 'go.sum']
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(n.relative_to(ROOT)): sha(n) for n in (main, adapter)},
     'instrumentation': 'Observe ApplyMessage result without changing its call or any branch in actual applyTransaction',
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False, 'scope': v['scope']}
target = ROOT / 'fixtures/protocol/precompiles-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('Precompile source evidence differs')
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'groups': v['groups'], 'runs': len(v['runs']),
                  'vectors': len(v['vectors'])}))
