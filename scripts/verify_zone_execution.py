#!/usr/bin/env python3
"""Qualify reward-depth execution and local state images without Go or peers."""
import datetime
import json
import os
import subprocess
from pathlib import Path
from lab import ROOT, sha, save

def main():
    build = json.loads((ROOT / 'results/rust-build.json').read_text())
    for group in ['rust_sources', 'source_assets']:
        for name, digest in build[group].items():
            assert sha(ROOT / name) == digest, 'Rebuild first: ' + name
    assert build['cargo_lock_sha256'] == sha(ROOT / 'Cargo.lock')
    out = ROOT / 'results' / ('zone-execution-' + datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'))
    out.mkdir(parents=True)
    target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).resolve()
    names = ['payouts', 'zone-blocks']
    with (out / 'build.log').open('w') as log:
        subprocess.run(['cargo', 'build', '--locked', '--offline', '--release', '-p', 'quai-consensus', *[v for n in names for v in ['--example', 'check-' + n]]], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True)
    reports = {}
    for name in names:
        corpus = ROOT / 'fixtures/protocol' / (name + '.json')
        evidence = corpus.with_name(name + '-source.json')
        manifest = json.loads(evidence.read_text())
        assert manifest['reference_revision'] == build['reference_revision']
        assert manifest['vectors_sha256'] == sha(corpus)
        driver = 'gen_payout_vectors.py' if name == 'payouts' else 'gen_zone_block_vectors.py'
        assert manifest['driver_sha256'] == sha(ROOT / 'scripts' / driver)
        if 'oracle_sources' in manifest:
            for path, digest in manifest['oracle_sources'].items():
                assert sha(ROOT / path) == digest
        else:
            assert manifest['oracle_source_sha256'] == sha(ROOT / 'reference/zone-blocks/main.go')
            assert manifest['adapter_sha256'] == sha(ROOT / 'reference/zone-blocks/core-export.go')
            assert manifest['harness_sha256'] == sha(ROOT / 'reference/early-blocks/harness.go')
        binary = target / 'release/examples' / ('check-' + name)
        result = subprocess.run([str(binary), str(corpus)], cwd=ROOT, text=True, capture_output=True)
        (out / (name + '.log')).write_text(result.stdout + result.stderr)
        assert result.returncode == 0, name
        report = json.loads(result.stdout)
        assert report['passed'], name
        save(out / (name + '.json'), report)
        reports[name] = {**{key: report[key] for key in ['passed', 'cases', 'checks']}, 'corpus_sha256': sha(corpus), 'evidence_sha256': sha(evidence), 'checker_sha256': sha(binary)}
    assert reports['payouts']['cases'] == 486 and reports['payouts']['checks'] == 1350
    assert reports['zone-blocks']['cases'] == 16 and reports['zone-blocks']['checks'] == 336
    summary = {'passed': True, 'reference_revision': build['reference_revision'], 'build_sha256': sha(ROOT / 'results/rust-build.json'), 'components': reports, 'native_work_blocks': 16, 'forged_execution_blocks_rejected_atomically': 112, 'complete_local_state_image_restores': 16, 'runtime_go_dependency': False, 'public_network': False, 'full_consensus': False, 'canonical_sync': False, 'scope': 'Exact extracted fee/reward branch projections plus actual Process on 16 linked private blocks with real native ProgPoW and restored account/ETX/UTXO state after every block. Inbound lists and placement are controlled oracle inputs. Local image payload identity is trusted; this is not network snapshot admission, durable canonical publication or full header/hierarchy validation.'}
    save(out / 'summary.json', summary)
    save(ROOT / 'results/latest-zone-execution.json', {'passed': True, 'directory': str(out.relative_to(ROOT))})
    print(json.dumps({'passed': True, 'directory': str(out.relative_to(ROOT)), 'components': reports}, indent=2))

if __name__ == '__main__':
    main()
