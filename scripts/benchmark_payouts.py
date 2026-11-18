#!/usr/bin/env python3
"""Fixed paired offline reward-distribution timings, after correctness checks."""
import argparse
import datetime
import json
import os
import platform
import statistics
import subprocess
import tarfile
from pathlib import Path
from lab import ROOT, sha, save

p = argparse.ArgumentParser()
p.add_argument('--baseline', type=Path, required=True)
p.add_argument('--baseline-target', type=Path, required=True)
p.add_argument('--candidate-target', type=Path, required=True)
a = p.parse_args()
baseline = a.baseline.resolve()
sources = {'baseline': baseline, 'candidate': ROOT}
targets = {'baseline': a.baseline_target.resolve(), 'candidate': a.candidate_target.resolve()}
old = json.loads((baseline / 'results/baseline-source.json').read_text())
for name, digest in old['files'].items():
    assert sha(baseline / name) == digest, name
assert '2 passed; 0 failed' in (baseline / 'results/baseline-correctness.log').read_text()
build = json.loads((ROOT / 'results/rust-build.json').read_text())
for group in ['rust_sources', 'source_assets']:
    for name, digest in build[group].items():
        assert sha(ROOT / name) == digest, name
assert build['reference_revision'] == old['reference_revision']
for name in ['Cargo.lock', 'fixtures/protocol/zone-blocks.json', 'crates/quai-consensus/examples/bench-payouts.rs']:
    assert sha(baseline / name) == sha(ROOT / name), name
latest = json.loads((ROOT / 'results/latest-zone-execution.json').read_text())
correctness = ROOT / latest['directory'] / 'summary.json'
c = json.loads(correctness.read_text())
assert c['passed'] and c['build_sha256'] == sha(ROOT / 'results/rust-build.json')
out = ROOT / 'results' / ('payout-performance-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'))
out.mkdir(parents=True)
for role in sources:
    env = {**os.environ, 'CARGO_NET_OFFLINE': 'true', 'CARGO_TARGET_DIR': str(targets[role])}
    with (out / (role + '-build.log')).open('w') as log:
        subprocess.run(['cargo', 'build', '--release', '--locked', '--offline', '-p', 'quai-consensus', '--example', 'bench-payouts'], cwd=sources[role], env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
raw = []
stable = None
for round_number in range(5):
    for role in (['baseline', 'candidate'] if round_number % 2 == 0 else ['candidate', 'baseline']):
        load = os.getloadavg()
        result = json.loads(subprocess.check_output([str(targets[role] / 'release/examples/bench-payouts'), str(ROOT / 'fixtures/protocol/zone-blocks.json')], cwd=sources[role], text=True))
        output = {k: v for k, v in result.items() if k != 'ns_per_operation'}
        if stable is None:
            stable = output
        else:
            assert stable == output, (role, round_number)
        raw.append({'round': round_number, 'role': role, 'load_average': load, 'result': result})
        save(out / 'raw.json', raw)
samples = {role: [r['result']['ns_per_operation'] for r in raw if r['role'] == role] for role in sources}
medians = {role: statistics.median(values) for role, values in samples.items()}
with tarfile.open(out / 'baseline-source.tar.gz', 'w:gz') as archive:
    for name in sorted(old['files']):
        archive.add(baseline / name, arcname=name, recursive=False)
    archive.add(baseline / 'results/baseline-source.json', arcname='results/baseline-source.json')
    archive.add(baseline / 'results/baseline-correctness.log', arcname='results/baseline-correctness.log')
for name, digest in old['files'].items():
    assert sha(baseline / name) == digest
for name, digest in build['rust_sources'].items():
    assert sha(ROOT / name) == digest
report = {'passed': True, 'reference_revision': build['reference_revision'], 'rounds': 5, 'sequential_timing': True, 'shared_host': True, 'host': {'system': platform.system(), 'machine': platform.machine(), 'cpu_count': os.cpu_count()}, 'samples_ns': samples, 'median_ns': medians, 'speedup': medians['baseline'] / medians['candidate'], 'workload': stable, 'fixture_sha256': sha(ROOT / 'fixtures/protocol/zone-blocks.json'), 'harness_sha256': sha(ROOT / 'crates/quai-consensus/examples/bench-payouts.rs'), 'driver_sha256': sha(Path(__file__).resolve()), 'candidate_build_sha256': sha(ROOT / 'results/rust-build.json'), 'baseline_archive_sha256': sha(out / 'baseline-source.tar.gz'), 'correctness_summary': str(correctness.relative_to(ROOT)), 'correctness_summary_sha256': sha(correctness), 'baseline_correctness_sha256': sha(baseline / 'results/baseline-correctness.log'), 'binaries': {role: sha(targets[role] / 'release/examples/bench-payouts') for role in sources}, 'source_sha256': {role: {str(f.relative_to(source)): sha(f) for f in sorted((source / 'crates').rglob('*.rs'))} for role, source in sources.items()}, 'cache_limits': {'entries': 512, 'logical_key_and_entry_bytes': 4194304, 'allocator_or_rss_measurement': False}, 'full_consensus': False, 'canonical_sync': False, 'scope': 'Prior Rust versus bounded positive inclusion-work memo on repeated identical reward distribution with a warm native epoch. No fresh-header, full replay, network sync, TPS, disk, allocation or peak-memory speedup is established.'}
save(out / 'summary.json', report)
save(ROOT / 'results/latest-payout-performance.json', {'passed': True, 'directory': str(out.relative_to(ROOT))})
print(json.dumps({'passed': True, 'directory': str(out.relative_to(ROOT)), 'median_ns': medians, 'speedup': report['speedup']}, indent=2))
