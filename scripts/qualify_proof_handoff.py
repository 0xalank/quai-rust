#!/usr/bin/env python3
"""Sequential, source-bound native proof-handoff qualification; no Go execution."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from lab import ROOT, save, sha

p = argparse.ArgumentParser()
p.add_argument('--small-oracle', type=Path, required=True)
p.add_argument('--large-oracle', type=Path, required=True)
p.add_argument('--compatibility-baseline', type=Path, required=True)
a = p.parse_args()
driver = sha(Path(__file__))
contract_path = ROOT / 'results/proof-handoff-contract.json'
contract = json.loads(contract_path.read_text())
assert contract['reference_revision'] == '33db82f2891b267ba898ca38bd2dd8bf75cbb072'
for name, digest in contract['source_inputs'].items():
    assert sha(ROOT / name) == digest, name
for oracle, count in [(a.small_oracle.resolve(), 96), (a.large_oracle.resolve(), 8192)]:
    m = json.loads((oracle / 'source.json').read_text())
    assert m['passed'] and m['blocks'] == count and m['validated_from_genesis']
    assert not m['checkpoint_imported'] and not m['initial_allocations']
    assert m['reference_revision'] == contract['reference_revision']
    assert sha(oracle / 'blocks.jsonl.gz') == m['compressed_stream_sha256']
env = {**os.environ, 'CARGO_NET_OFFLINE': 'true'}
stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
out = ROOT / 'results' / ('proof-handoff-qualification-' + stamp)
out.mkdir()
save(out / 'contract.json', contract)
(out / 'qualification-driver.py').write_bytes(Path(__file__).read_bytes())
python = [sys.executable, '-B']
commands = [
    ('standalone-build', python + ['scripts/build.py']),
    ('native-transport', ['cargo', 'test', '--release', '--locked', '--offline', '-p',
                          'quai-network', '--features', 'native-transport', '--lib', '--', '--nocapture']),
    ('genesis-replay', python + ['scripts/verify_genesis_stream.py', '--oracle', str(a.large_oracle.resolve())]),
    ('forward-processes', python + ['scripts/verify_forward_processes.py', '--small-oracle',
                                   str(a.small_oracle.resolve()), '--large-oracle', str(a.large_oracle.resolve())]),
    *[(name, python + ['scripts/verify_' + name.replace('-', '_') + '.py']) for name in
      ['coincident', 'activation', 'persistent-execution', 'state-db', 'stateful', 'zone-execution',
       'private-branches', 'admission', 'confirmation', 'uncles', 'weakshares']],
    ('canonical', python + ['scripts/verify_canonical.py', '--peer']),
    ('private-processes', python + ['scripts/verify_private_processes.py']),
    ('rpc-sdk', python + ['scripts/verify_rust.py', '--sdk']),
    ('no-go', python + ['scripts/verify_rust_build.py']),
    ('private-node-bundle', python + ['scripts/build_private_node.py']),
    ('same-format-compatibility', python + ['scripts/verify_private_aux_compatibility.py', '--baseline',
                                          str(a.compatibility_baseline.resolve())]),
]
steps = []
for name, command in commands:
    print(json.dumps({'checking': name}), flush=True)
    log = out / (name + '.log')
    with log.open('w') as stream:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=stream, stderr=subprocess.STDOUT)
    step = {'name': name, 'command': command, 'exit_code': result.returncode,
            'log': str(log.relative_to(ROOT)), 'log_sha256': sha(log)}
    steps.append(step)
    save(out / 'steps.json', steps)
    assert result.returncode == 0, str(log)
    print(json.dumps({'passed': name}), flush=True)
assert sha(Path(__file__)) == driver and sha(contract_path) == sha(out / 'contract.json')
for name, digest in contract['source_inputs'].items():
    assert sha(ROOT / name) == digest, name
build = json.loads((ROOT / 'results/rust-build.json').read_text())
for group in ['rust_sources', 'source_assets']:
    for name, digest in build[group].items():
        assert sha(ROOT / name) == digest, name
tests = sum(map(int, re.findall(r'test result: ok\. (\d+) passed', (out / 'standalone-build.log').read_text())))
native_log = (out / 'native-transport.log').read_text()
assert 'test result: ok. 13 passed' in native_log
comparison = next(json.loads(line) for line in native_log.splitlines()
                  if line.startswith('{') and 'local-header-proof-handoff-comparison' in line)
assert comparison['blocks'] == 16 and comparison['same_native_roots']
assert comparison['baseline_admission_misses'] == 24 and comparison['candidate_admission_misses'] == 8
assert comparison['reused_header_proofs'] == 16 and not comparison['timing_measured']
no_go = json.loads((ROOT / 'results/rust-no-go-build.json').read_text())
assert no_go['passed'] and no_go['rust_tests'] == tests == 259
assert no_go['sealed_local_header_proof_handoff_tests_executed']
assert no_go['go_commands_attempted'] == no_go['protoc_commands_attempted'] == 0
proofs = {}
for name in ['genesis-replay', 'forward-processes', 'coincident', 'activation', 'persistent-execution',
             'state-db', 'stateful', 'zone-execution', 'private-branches', 'admission', 'confirmation',
             'uncles', 'weakshares', 'canonical', 'private-processes', 'rust', 'private-aux-compatibility']:
    pointer = json.loads((ROOT / ('results/latest-' + name + '.json')).read_text())
    path = ROOT / pointer['directory'] / 'summary.json'
    summary = json.loads(path.read_text())
    assert summary['passed'] and summary['build_sha256'] == sha(ROOT / 'results/rust-build.json'), name
    proofs[str(path.relative_to(ROOT))] = sha(path)
forward_pointer = json.loads((ROOT / 'results/latest-forward-processes.json').read_text())
forward = json.loads((ROOT / forward_pointer['directory'] / 'summary.json').read_text())
assert forward['small_forward_result']['reused_header_proofs'] >= 96
assert forward['large_forward_result']['reused_header_proofs'] >= 8192
report = {'passed': True, 'source_build_sha256': sha(ROOT / 'results/rust-build.json'),
          'reference_revision': contract['reference_revision'], 'rust_tests': tests,
          'native_transport_tests': 13, 'logical_proof_comparison': comparison,
          'forward_reused_header_proofs': forward['large_forward_result']['reused_header_proofs'],
          'evidence': proofs, 'steps_sha256': sha(out / 'steps.json'),
          'driver_sha256': driver, 'contract_sha256': sha(contract_path),
          'runtime_go_dependency': False, 'timing_measured': False, 'heap_or_rss_measured': False,
          'full_consensus': False, 'canonical_public_sync': False, 'goal_status': 'active'}
save(out / 'summary.json', report)
save(ROOT / 'results/latest-proof-handoff-qualification.json',
     {'passed': True, 'directory': str(out.relative_to(ROOT))})
print(json.dumps(report, indent=2), flush=True)
