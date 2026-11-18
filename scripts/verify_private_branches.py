#!/usr/bin/env python3
"""Native private branch/state recovery against frozen Go output. No Go or peers."""
import datetime
import json
import os
import subprocess
import tempfile
from pathlib import Path
from lab import ROOT, sha, save

build = json.loads((ROOT / 'results/rust-build.json').read_text())
for group in ['rust_sources', 'source_assets']:
    for name, digest in build[group].items():
        assert sha(ROOT / name) == digest, 'Rebuild source first: ' + name
assert build['cargo_lock_sha256'] == sha(ROOT / 'Cargo.lock')
corpus = ROOT / 'fixtures/protocol/private-branches.json'
evidence = corpus.with_name('private-branches-source.json')
manifest = json.loads(evidence.read_text())
assert manifest['reference_revision'] == build['reference_revision']
assert manifest['vectors_sha256'] == sha(corpus)
assert manifest['driver_sha256'] == sha(ROOT / 'scripts/gen_private_branch_vectors.py')
assert manifest['oracle_source_sha256'] == sha(ROOT / 'reference/private-branches/main.go')
assert manifest['adapter_sha256'] == sha(ROOT / 'reference/private-branches/core-export.go')
assert manifest['harness_sha256'] == sha(ROOT / 'reference/early-blocks/harness.go')
out = ROOT / 'results' / ('private-branches-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'))
out.mkdir(parents=True)
target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).resolve()
with (out / 'build.log').open('w') as log:
    subprocess.run(['cargo', 'build', '--release', '--locked', '--offline', '-p', 'quai-consensus', '--example', 'check-private-branches'], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True)
binary = target / 'release/examples/check-private-branches'
with tempfile.TemporaryDirectory(prefix='quai-branch-qualification-') as directory:
    result = subprocess.run([str(binary), str(corpus), directory], cwd=ROOT, text=True, capture_output=True)
(out / 'check.log').write_text(result.stdout + result.stderr)
assert result.returncode == 0, 'Native private branch mismatch'
report = json.loads(result.stdout)
assert report['passed'] and report['unique_native_blocks_executed'] == 24
assert report['checks'] == 289 and report['branch_switches'] == 2
save(out / 'checks.json', report)
summary = {**report, 'reference_revision': build['reference_revision'], 'build_sha256': sha(ROOT / 'results/rust-build.json'), 'corpus_sha256': sha(corpus), 'evidence_sha256': sha(evidence), 'checker_sha256': sha(binary), 'private_profile_sha256': sha(ROOT / 'configs/private-native-v1.json'), 'public_network': False, 'production_state_database': False, 'private_commit_limit_bytes': 63 << 20, 'recovery_history_record_limit': 4096, 'scope': 'Two Go-derived execution-only private branches. Complete account/ETX/raw-lockup/UTXO image, typed receipt bytes and branch-specific execution history persist in the private journal. Native work hashes match Go. Reopens, explicit switches and corrupt/stale rejection pass; full header/hierarchy/inbound eligibility and PoEM canonical sync remain unavailable.'}
save(out / 'summary.json', summary)
save(ROOT / 'results/latest-private-branches.json', {'passed': True, 'directory': str(out.relative_to(ROOT))})
print(json.dumps({'passed': True, 'directory': str(out.relative_to(ROOT)), 'unique_native_blocks_executed': 24, 'checks': report['checks'], 'full_consensus': False, 'canonical_sync': False}, indent=2))
