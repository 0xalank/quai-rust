"""Build local PoW/test-transfer driver and explicitly funded local-fixture CLI."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
from lab import ROOT, save, sha


def build(go_quai):
    go_quai = Path(go_quai).resolve()
    miner = ROOT / 'reference/localmine/main.go'
    fixture = ROOT / 'reference/localfixture/main.go'
    subprocess.run(['gofmt', '-w', str(miner), str(fixture)], check=True)
    with tempfile.TemporaryDirectory(prefix='quai-local-build-') as directory:
        overlay = Path(directory) / 'overlay.json'
        overlay.write_text(json.dumps({'Replace': {
            str(go_quai / 'cmd/quai-local-mine/main.go'): str(miner),
            str(go_quai / 'cmd/go-quai/local_fixture.go'): str(fixture),
        }}))
        for target, source in [('quai-local-mine', './cmd/quai-local-mine'),
                               ('go-quai-local-fixture', './cmd/go-quai')]:
            subprocess.run(['go', 'build', '-overlay', str(overlay), '-o', str(ROOT / 'bin' / target), source], cwd=go_quai, check=True)
    save(ROOT / 'results/live-build.json', {
        'go_quai_revision': subprocess.check_output(['git','rev-parse','HEAD'],cwd=go_quai,text=True).strip(),
        'source_hashes': {str(p.relative_to(ROOT)):sha(p) for p in [miner,fixture]},
        'binaries':{p:sha(ROOT/'bin'/p) for p in ['quai-local-mine','go-quai-local-fixture']},
        'scope':'Local-only blake3 chain; funded CLI changes allocation/genesis identity and TimeToStartTx=0. No production keys or broadcasts.',
    })


if __name__ == '__main__':
    build(os.environ.get('GO_QUAI_DIR', ROOT.parent / 'go-quai'))
