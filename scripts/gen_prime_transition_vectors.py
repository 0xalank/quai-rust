#!/usr/bin/env python3
"""Explicit offline oracle: linked prime blocks through the official prime
branch of Slice.Append across ControllerKickInBlock, the KQuaiChange rows
and their holds."""
import argparse, hashlib, json, os, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

NAMES = ['reference/prime-transition/main.go', 'reference/prime-transition/core-template.go',
         'reference/prime-transition/LICENSE', 'reference/prime-transition/NOTICE.md']
SOURCES = ['core/slice.go', 'core/exchange_controller.go', 'core/headerchain.go', 'core/state_processor.go',
           'consensus/misc/rewards.go', 'common/big.go', 'params/protocol_params.go', 'core/bodydb.go',
           'core/types/block.go', 'core/types/wo.go', 'core/types/token_choice.go', 'core/types/transaction.go',
           'core/rawdb/accessors_chain.go', 'go.mod', 'go.sum']


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--go-quai', required=True)
    mode = p.add_mutually_exclusive_group(required=True)
    mode.add_argument('--write', action='store_true')
    mode.add_argument('--check', action='store_true')
    a = p.parse_args()
    go = Path(a.go_quai).resolve()
    checked_reference(go)
    source = go / 'core/slice.go'
    text = source.read_text()
    start = text.index('\tif nodeCtx == common.PRIME_CTX {\n\t\t// Until the controller has kicked in')
    stop = text.index('\n\ttime5 :=', start)
    branch = text[start:stop]
    template = ROOT / NAMES[1]
    body = template.read_text()
    assert body.count('@@APPEND_BRANCH@@') == 1
    rendered = body.replace('@@APPEND_BRANCH@@', branch)
    out = ROOT / 'results'
    out.mkdir(exist_ok=True)
    generated = out / 'prime-transition-reference-core.go'
    generated.write_text(rendered)
    env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
    with tempfile.TemporaryDirectory(prefix='quai-prime-transition-oracle-') as directory:
        d = Path(directory)
        overlay = d / 'overlay.json'
        binary = d / 'oracle'
        overlay.write_text(json.dumps({'Replace': {
            str(go / 'cmd/quai-prime-transition-vectors/main.go'): str(ROOT / NAMES[0]),
            str(go / 'core/native_prime_transition_vectors.go'): str(generated)}}))
        with (out / 'prime-transition-oracle-build.log').open('w') as log:
            subprocess.run(['go', 'build', '-overlay', str(overlay), '-ldflags', '-X main.referenceRevision=' + PIN,
                            '-o', str(binary), './cmd/quai-prime-transition-vectors'],
                           cwd=go, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
        result = subprocess.run([str(binary)], cwd=d, env=env, capture_output=True)
        (out / 'prime-transition-oracle.stdout').write_bytes(result.stdout)
        (out / 'prime-transition-oracle.stderr').write_bytes(result.stderr)
        result.check_returncode()
        value = json.loads(result.stdout)
    assert value['reference_revision'] == PIN and len(value['segments']) == 9
    blocks = sum(len(s['blocks']) for s in value['segments'])
    assert value['accepted_blocks'] == blocks
    corpus = ROOT / 'fixtures/protocol/prime-transition.json'
    encoded = (json.dumps(value, indent=1, sort_keys=True) + '\n').encode()
    digest = lambda s: hashlib.sha256(s.encode()).hexdigest()
    evidence = {
        'reference_revision': PIN, 'driver_sha256': sha(Path(__file__)),
        'oracle_sources': {n: sha(ROOT / n) for n in NAMES}, 'sources': {n: sha(go / n) for n in SOURCES},
        'scope': value['scope'], 'runtime_go_dependency': False, 'canonical_authority': False,
        'segments': [s['name'] for s in value['segments']], 'accepted_blocks': blocks,
        'go_rejected_field_changes': value['go_rejected_field_changes'],
        'go_rejected_history': value['go_rejected_history'], 'go_accepted_history': value['go_accepted_history'],
        'append_branch_range_chars': [start, stop], 'append_branch_sha256': digest(branch),
        'append_branch_byte_identical': True}
    if a.write:
        corpus.write_bytes(encoded)
    else:
        assert corpus.read_bytes() == encoded, 'Prime transition corpus differs'
    evidence['vectors_sha256'] = sha(corpus)
    meta = corpus.with_name('prime-transition-source.json')
    if a.write:
        meta.write_text(json.dumps(evidence, indent=2) + '\n')
    else:
        assert json.loads(meta.read_text()) == evidence, 'Prime transition source differs'
    print(json.dumps({'passed': True, 'segments': len(value['segments']), 'accepted_blocks': blocks,
                      'go_rejected_field_changes': value['go_rejected_field_changes'],
                      'go_rejected_history': value['go_rejected_history'],
                      'go_accepted_history': value['go_accepted_history'],
                      'priced_etxs': value['priced_etxs']}))


if __name__ == '__main__':
    main()
