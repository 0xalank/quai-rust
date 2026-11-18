#!/usr/bin/env python3
"""Live check of the transaction pool on a replayed mainnet state.

Reads the head of a local `quai-node rpc` (read-only calls), rebuilds the
fields the pool reads in go-quai v0.57.0's production pool (the oracle's
live mode, reference/txpool/live.go), lets Go answer a list of signed test
transactions from keys that never held funds, then submits the same bytes
to the Rust node's quai_sendRawTransaction and compares every answer.

Safety: the Rust node must listen on a loopback address; port 9200 (the
local go-quai node) is refused. Nothing is sent anywhere else.

    scripts/check_txpool_live.py --rpc http://127.0.0.1:8547 --go-quai ../go-quai
"""
import argparse, json, os, subprocess, sys, tempfile, time, urllib.request
from pathlib import Path
from urllib.parse import urlparse
from gen_foundation_vectors import ROOT, PIN, checked_reference

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--rpc', required=True)
p.add_argument('--go-quai', required=True)
p.add_argument('--out', default=str(ROOT / 'results/txpool/live.json'))
a = p.parse_args()
u = urlparse(a.rpc)
if u.scheme != 'http' or u.hostname not in ('127.0.0.1', 'localhost', '::1') or u.port in (None, 9200):
    raise SystemExit('refusing: the Rust node must be a loopback http endpoint, not port 9200')

ident = 0
def call(method, params):
    global ident
    ident += 1
    body = json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params}).encode()
    req = urllib.request.Request(a.rpc, data=body, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=60) as r:
        answer = json.loads(r.read())
    return {k: answer[k] for k in ('result', 'error') if k in answer}

head = int(call('quai_blockNumber', [])['result'], 16)
block = call('quai_getBlockByNumber', [hex(head), False])['result']
wo, h = block['woHeader'], block['header']
prime = call('quai_getBlockByHash', [h['primeTerminusHash'], False])['result']
# UTXOs of a Qi coinbase near the head, if any (another owner's UTXOs).
utxos = []
for n in range(head, max(head - 2000, 0), -1):
    b = call('quai_getBlockByNumber', [hex(n), False])['result']
    cb = b['woHeader']['primaryCoinbase']
    if int(cb[4:6], 16) >= 0x80:
        for o in call('quai_getOutpointsByAddress', [cb, 'latest']).get('result') or []:
            utxos.append({'hash': o['txHash'], 'index': int(o['index'], 16),
                          'denomination': int(o['denomination'], 16), 'address': cb.lower(),
                          'lock': o.get('lock') or '0x'})
        if utxos:
            break
params = {
    'number': head, 'base_fee': h.get('baseFeePerGas', '0x0'), 'gas_limit': int(h['gasLimit'], 16),
    'difficulty': wo['difficulty'], 'prime_terminus_number': int(wo['primeTerminusNumber'], 16),
    'expansion_number': int(h['expansionNumber'], 16), 'avg_tx_fees': h.get('avgTxFees', '0x0'),
    'exchange_rate': prime['header']['exchangeRate'],
    'etx_eligible_slices': prime['header']['etxEligibleSlices'],
    'utxo_set_size': 0, 'utxos': utxos[:2],
}
go = Path(a.go_quai).resolve()
checked_reference(go)
ref = ROOT / 'reference/txpool'
files = {'cmd/quai-txpool-vectors/main.go': ref / 'main.go',
         'cmd/quai-txpool-vectors/scenarios.go': ref / 'scenarios.go',
         'cmd/quai-txpool-vectors/live.go': ref / 'live.go',
         'core/txpool_oracle_export.go': ref / 'core-export.go',
         'core/types/txpool_oracle_export.go': ref / 'types-export.go'}
env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
with tempfile.TemporaryDirectory(prefix='quai-txpool-live-') as directory:
    d = Path(directory)
    (d / 'overlay.json').write_text(json.dumps({'Replace': {str(go / k): str(v) for k, v in files.items()}}))
    (d / 'params.json').write_text(json.dumps(params))
    subprocess.run(['go', 'build', '-overlay', str(d / 'overlay.json'), '-ldflags',
                    '-X main.referenceRevision=' + PIN, '-o', str(d / 'oracle'),
                    './cmd/quai-txpool-vectors'], cwd=go, env=env, check=True)
    oracle = json.loads(subprocess.check_output([str(d / 'oracle'), 'live', str(d / 'params.json')], cwd=d, env=env))

results, equal = [], 0
for step in oracle['steps']:
    if step['op'] == 'start':
        continue
    if step['op'] == 'run':
        time.sleep(1.5)  # the node's pool runs every second
        continue
    if step['op'] == 'send':
        got = call('quai_sendRawTransaction', [step['raw']])
        method, params_ = 'quai_sendRawTransaction', [step['raw'][:18] + '...']
    else:
        got = call(step['method'], step['params'])
        method, params_ = step['method'], step['params']
    ok = got == step['answer']
    equal += ok
    results.append({'method': method, 'params': params_, 'note': step.get('note', ''),
                    'go': step['answer'], 'rust': got, 'equal': ok})
summary = {'rpc': a.rpc, 'head': head, 'params': params, 'reference_revision': PIN,
           'compared': len(results), 'equal': equal, 'results': results}
Path(a.out).parent.mkdir(parents=True, exist_ok=True)
Path(a.out).write_text(json.dumps(summary, indent=1) + '\n')
for r in results:
    if not r['equal']:
        print('DIFF', r['method'], r['note'], json.dumps(r['go'])[:200], json.dumps(r['rust'])[:200])
print(json.dumps({'head': head, 'compared': len(results), 'equal': equal, 'utxos': len(params['utxos'])}))
sys.exit(0 if equal == len(results) else 1)
