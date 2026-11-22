#!/usr/bin/env python3
"""Go oracle for the block and header views a node answers to a peer's
WorkObjectBlock and WorkObjectHeader requests (go-quai v0.57.0
p2p/protocol/handler.go handleBlockRequest: ConvertToBlockView and
ConvertToHeaderView, then ProtoEncode), over stored work objects of every
context: the private hierarchy fixture (fixtures/protocol/canonical.json)
and, with --export, prime, region and zone records of a mainnet QEX1
segment (blocks after KawPow). Explicit and offline; never part of the Rust
runtime.

    scripts/gen_header_view_vectors.py --go-quai DIR --write --export EXPORT --segment FILE
    scripts/gen_header_view_vectors.py --go-quai DIR --check
"""
import argparse, json, os, struct, subprocess, tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT, PIN, checked_reference, sha

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--go-quai', required=True)
p.add_argument('--export', help='a QEX1 export directory (with --write)')
p.add_argument('--segment', help='a segment file name in it (with --write)')
p.add_argument('--per-context', type=int, default=12)
m = p.add_mutually_exclusive_group(required=True)
m.add_argument('--write', action='store_true')
m.add_argument('--check', action='store_true')
a = p.parse_args()
go = Path(a.go_quai).resolve()
checked_reference(go)
path = ROOT / 'fixtures/protocol/header-views.json'


def fixture_inputs():
    doc = json.loads((ROOT / 'fixtures/protocol/canonical.json').read_text())
    out = []
    for b, branch in enumerate(doc['branches']):
        for i, v in enumerate(branch['vectors']):
            for c in v['contexts']:
                out.append({'name': f'canonical/branch{b}/height{i + 1}/ctx{c["context"]}',
                            'context': c['context'], 'block': c['work_object']})
    return out


def mainnet_inputs(export, segment, per_context):
    out, counts = [], {0: 0, 1: 0, 2: 0}
    with open(Path(export) / 'segments' / segment, 'rb') as f:
        assert f.read(5) == b'QEX1\n'
        while min(counts.values()) < per_context:
            head = f.read(1 + 1 + 8 + 32 + 4)
            if len(head) < 46:
                break
            kind, ctx, number, h, n = struct.unpack('>BBQ32sI', head)
            block = f.read(n)
            (r,) = struct.unpack('>I', f.read(4))
            f.seek(r, 1)
            if kind == 1 and counts[ctx] < per_context:
                counts[ctx] += 1
                out.append({'name': f'mainnet/ctx{ctx}/{number}/0x{h.hex()[:16]}',
                            'context': ctx, 'block': '0x' + block.hex()})
    return out


if a.write:
    inputs = fixture_inputs()
    if a.export:
        inputs += mainnet_inputs(a.export, a.segment, a.per_context)
else:
    inputs = [{'name': c['name'], 'context': c['context'], 'block': c['block']}
              for c in json.loads(path.read_text())['cases']]
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
source = ROOT / 'reference/header-views/main.go'
(ROOT / 'target').mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='quai-header-views-', dir=ROOT / 'target') as directory:
    d = Path(directory)
    overlay = d / 'overlay.json'
    binary = d / 'oracle'
    overlay.write_text(json.dumps({'Replace': {str(go / 'cmd/quai-header-views/main.go'): str(source)}}))
    subprocess.run(['nice', '-n', '19', 'go', 'build', '-p', '2', '-overlay', str(overlay),
                    '-ldflags', '-X main.referenceRevision=' + PIN, '-o', str(binary),
                    './cmd/quai-header-views'], cwd=go, env=env, check=True)
    (d / 'inputs.json').write_text(json.dumps(inputs))
    raw = subprocess.check_output(['nice', '-n', '19', str(binary), str(d / 'inputs.json')], cwd=d, env=env)
v = json.loads(raw)
assert v['reference_revision'] == PIN, v['reference_revision']
assert len(v['cases']) == len(inputs)
encoded = (json.dumps(v, indent=1, sort_keys=True) + '\n').encode()
if a.write:
    path.write_bytes(encoded)
elif path.read_bytes() != encoded:
    raise SystemExit('header view corpus differs')
names = ['core/types/wo.go', 'core/types/proto_block.pb.go', 'p2p/protocol/handler.go',
         'p2p/node/api.go', 'p2p/pb/proto_services.go', 'go.mod', 'go.sum']
e = {'reference_revision': PIN, 'vectors_sha256': sha(path),
     'driver_sha256': sha(Path(__file__).resolve()),
     'oracle_sources': {str(source.relative_to(ROOT)): sha(source)},
     'go_sources': {n: sha(go / n) for n in names},
     'public_network': False, 'runtime_go_dependency': False}
target = ROOT / 'fixtures/protocol/header-views-source.json'
if a.write:
    target.write_text(json.dumps(e, indent=2) + '\n')
elif json.loads(target.read_text()) != e:
    raise SystemExit('header view source evidence differs')
errors = sum(1 for c in v['cases'] if c.get('error'))
print(json.dumps({'passed': True, 'mode': 'write' if a.write else 'check',
                  'cases': len(v['cases']), 'errors': errors,
                  'by_context': {k: sum(1 for c in v['cases'] if c['context'] == k) for k in (0, 1, 2)}}))
