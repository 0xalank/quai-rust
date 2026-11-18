#!/usr/bin/env python3
"""Build the oracle inside go-quai's import boundary using a temporary Go overlay."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile

from lab import ROOT, save, sha


def build(go_quai):
    go_quai = Path(go_quai).resolve()
    source = ROOT / 'reference/chain/main.go'
    subprocess.run(['gofmt', '-w', str(source)], check=True)
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=go_quai, text=True).strip()
    diff = subprocess.check_output(['git', 'diff', 'HEAD', '--', '*.go', 'go.mod', 'go.sum'], cwd=go_quai)
    # Include tracked source content, not just HEAD: local edits affect the oracle.
    paths = subprocess.check_output(['git', 'ls-files', '-z'], cwd=go_quai).decode().split('\0')
    source_hashes = {p: sha(go_quai / p) for p in paths
                     if p and (p.endswith('.go') or p in ('go.mod', 'go.sum', 'VERSION'))}
    (ROOT / 'bin').mkdir(exist_ok=True)
    shutil.copy2(go_quai / 'VERSION', ROOT / 'reference/chain/VERSION')
    with tempfile.TemporaryDirectory(prefix='quai-oracle-overlay-') as directory:
        overlay = Path(directory) / 'overlay.json'
        overlay.write_text(json.dumps({'Replace': {
            str(go_quai / 'cmd/quai-rust-oracle/main.go'): str(source),
        }}))
        subprocess.run([
            'go', 'build', '-overlay', str(overlay),
            '-ldflags', '-X main.referenceRevision=' + revision,
            '-o', str(ROOT / 'bin/quai-chain-oracle'), './cmd/quai-rust-oracle',
        ], cwd=go_quai, check=True)
    metadata = {
        'built_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'go_quai_revision': revision, 'go_quai_tracked_dirty': bool(diff),
        'go_quai_diff_sha256': hashlib.sha256(diff).hexdigest(),
        'go_quai_source_hashes': source_hashes,
        'oracle_source_sha256': sha(source),
        'oracle_binary_sha256': sha(ROOT / 'bin/quai-chain-oracle'),
        'go_version': subprocess.check_output(['go', 'version'], text=True).strip(),
        'scope': 'Actual Go API handlers over seeded StateDB/UTXO/EVM. No synchronized or PoW-validated chain.',
    }
    save(ROOT / 'results/chain-build.json', metadata)
    print('Built bin/quai-chain-oracle (Go source tree unchanged)')
    return metadata


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--go-quai', default=os.environ.get('GO_QUAI_DIR', str(ROOT.parent / 'go-quai')))
    build(parser.parse_args().go_quai)
