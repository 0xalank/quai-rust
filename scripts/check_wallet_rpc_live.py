#!/usr/bin/env python3
"""Compare wallet read methods of the native RPC with a live go-quai node,
read-only: quai_qiToQuai at historical blocks (a header and its own prime
terminus), quai_getOutpointDeltasForAddressesInRange over historical ranges
for the Qi coinbase recipients of those blocks, quai_getLockedBalance of Qi
addresses, quai_listRunningChains and the argument errors of these methods.

The two nodes may have different heads: only answers that do not depend on
the current head are compared. Methods that read the current head
(quai_quaiToQi, quai_calculateConversionAmount, quai_estimateFeeForQi) are
covered by the Go oracle (fixtures/protocol/qi-rpc.json) instead.

    scripts/check_wallet_rpc_live.py --rust http://127.0.0.1:8547 \\
        --go http://127.0.0.1:9200 --out results/rpc-complete/live-wallet.json

Only the methods in READ are ever sent.
"""
import argparse, json, random, urllib.request

READ = {'quai_blockNumber', 'quai_getBlockByNumber', 'quai_qiToQuai',
        'quai_getOutpointDeltasForAddressesInRange', 'quai_getLockedBalance',
        'quai_listRunningChains', 'net_listening', 'net_version', 'quai_chainId'}
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--rust', required=True)
p.add_argument('--go', required=True)
p.add_argument('--out', required=True)
p.add_argument('--samples', type=int, default=60)
p.add_argument('--seed', type=int, default=7)
a = p.parse_args()
rng = random.Random(a.seed)


def call(url, method, params):
    assert method in READ, method
    body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params}).encode()
    req = urllib.request.Request(url, body, {'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=60) as r:
        v = json.loads(r.read())
    v.pop('id', None)
    return v


def num(h):
    return int(h, 16)


rust_head = num(call(a.rust, 'quai_blockNumber', [])['result'])
go_head = num(call(a.go, 'quai_blockNumber', [])['result'])
assert go_head >= rust_head, (go_head, rust_head)
cases = []


def compare(group, method, params):
    r, g = call(a.rust, method, params), call(a.go, method, params)
    cases.append({'group': group, 'method': method, 'params': params,
                  'equal': r == g, 'rust': r, 'go': g})


for m in ['quai_listRunningChains', 'net_listening', 'net_version', 'quai_chainId']:
    compare('identity', m, [])

# Qi coinbase recipients and sample blocks.
numbers = sorted(rng.sample(range(1, rust_head + 1), min(a.samples, rust_head)))
recipients = {}
for n in numbers:
    b = call(a.rust, 'quai_getBlockByNumber', [hex(n), True])['result']
    for tx in b['transactions']:
        to = tx.get('to')
        if tx.get('type') == '0x1' and to and int(to[4:6], 16) >= 0x80:
            recipients.setdefault(to, set()).add(n)
    for amount in ['0x0', '0x1', '0x3e8', '0xf4240', '0x3b9aca00', '0xde0b6b3a7640000']:
        compare('qiToQuai', 'quai_qiToQuai', [amount, hex(n)])
    compare('qiToQuai', 'quai_qiToQuai', ['0x3e8', {'blockHash': b['hash']}])

addresses = sorted(recipients)
for n in numbers[:20]:
    lo = max(0, n - rng.randrange(0, 1000))
    blo = call(a.rust, 'quai_getBlockByNumber', [hex(lo), False])['result']
    bhi = call(a.rust, 'quai_getBlockByNumber', [hex(n), False])['result']
    pick = [x for x in addresses if any(lo <= m <= n for m in recipients[x])][:5] or addresses[:3]
    if pick:
        compare('deltas', 'quai_getOutpointDeltasForAddressesInRange',
                [pick, blo['hash'], bhi['hash']])
if addresses:
    h1 = call(a.rust, 'quai_getBlockByNumber', ['0x1', False])['result']['hash']
    h2 = call(a.rust, 'quai_getBlockByNumber', [hex(min(rust_head, 2000)), False])['result']['hash']
    for params in ([[], h1, h1], [addresses[:1], h2, h1], [addresses[:1], h1, h2],
                   [addresses[:1], '0x' + '11' * 32, h1]):
        compare('deltas-errors', 'quai_getOutpointDeltasForAddressesInRange', params)
for addr in addresses[:20]:
    compare('lockedBalance', 'quai_getLockedBalance', [addr])
compare('lockedBalance', 'quai_getLockedBalance', ['0x1100000000000000000000000000000000000001'])

groups = {}
for c in cases:
    g = groups.setdefault(c['group'], [0, 0])
    g[0] += 1
    g[1] += c['equal']
out = {'rust_head': rust_head, 'go_head': go_head, 'groups': groups,
       'equal': sum(c['equal'] for c in cases), 'total': len(cases), 'cases': cases}
with open(a.out, 'w') as f:
    json.dump(out, f, indent=1)
print(json.dumps({k: out[k] for k in ['rust_head', 'go_head', 'groups', 'equal', 'total']}))
