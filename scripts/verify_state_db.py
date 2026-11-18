#!/usr/bin/env python3
"""Verify incremental secure tries and durable state versions against frozen Go outputs."""
import datetime, json, os, subprocess, tempfile
from pathlib import Path
from lab import ROOT, save, sha

def main():
    build = json.loads((ROOT/'results/rust-build.json').read_text())
    for group in ['rust_sources', 'source_assets']:
        for name, digest in build[group].items():
            assert sha(ROOT/name) == digest, 'Rebuild first: '+name
    assert build['cargo_lock_sha256'] == sha(ROOT/'Cargo.lock')
    corpus = ROOT/'fixtures/protocol/state-db.json'
    evidence = corpus.with_name('state-db-source.json')
    manifest = json.loads(evidence.read_text())
    assert manifest['reference_revision'] == build['reference_revision']
    assert manifest['vectors_sha256'] == sha(corpus)
    assert manifest['driver_sha256'] == sha(ROOT/'scripts/gen_state_db_vectors.py')
    assert manifest['oracle_sha256'] == sha(ROOT/'reference/state-db/main.go')
    output = ROOT/'results'/('state-db-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'))
    output.mkdir(parents=True)
    target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT/'target')).resolve()
    with (output/'build.log').open('w') as log:
        subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-store','--example','check-state-db'], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True)
    binary = target/'release/examples/check-state-db'
    with tempfile.TemporaryDirectory(prefix='quai-state-db-') as directory:
        run = subprocess.run([str(binary), str(corpus), str(Path(directory)/'state.redb')], cwd=ROOT, text=True, capture_output=True, timeout=180)
    (output/'check.log').write_text(run.stdout+run.stderr)
    assert run.returncode == 0, run.stderr[-2000:]
    result = json.loads(run.stdout)
    assert result['passed'] and result['checks'] == 6290
    assert result['bulk_keys'] == 70000 and result['long_history_commits'] == 5000
    assert result['state_versions'] == 5063 and result['go_mutation_steps'] == 1512
    assert not result['block_validation'] and not result['global_full_consensus'] and not result['canonical_public_sync']
    save(output/'summary.json', {**result, 'reference_revision':build['reference_revision'], 'build_sha256':sha(ROOT/'results/rust-build.json'), 'corpus_sha256':sha(corpus), 'evidence_sha256':sha(evidence), 'checker_sha256':sha(binary), 'qualification_driver_sha256':sha(Path(__file__)), 'public_network':False, 'canonical_execution_uses_state_database':False, 'scope':'Secure-trie updates/deletes/roots/lookups match actual pinned Go. Durable state versions, immutable forks, pinned snapshots and ancestor lookups are storage qualification, not admitted blocks or canonical client replay. Physical I/O, RSS, pruning and full-state execution are not qualified.'})
    save(ROOT/'results/latest-state-db.json', {'passed':True,'directory':str(output.relative_to(ROOT))})
    print(json.dumps({'passed':True,'directory':str(output.relative_to(ROOT)), 'checks':result['checks'], 'state_versions':result['state_versions']},indent=2))

if __name__ == '__main__':
    main()
