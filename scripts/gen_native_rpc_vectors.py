#!/usr/bin/env python3
"""Record go-quai v0.57.0 JSON-RPC answers as golden fixtures for the native
RPC mode (`quai-node rpc`, crates/quai-rpc/src/native).

    scripts/gen_native_rpc_vectors.py --head 8 --out fixtures/native-rpc/zone-0-8.json
    scripts/gen_native_rpc_vectors.py --head 27406 --out fixtures/native-rpc/zone-0-27406.json
    scripts/gen_native_rpc_vectors.py --calls --head 27406 --out fixtures/native-rpc/calls-0-27406.json

The Go node must serve zone [0 0] at --go (default http://127.0.0.1:9200,
read-only methods only) and must have synced past --head. --head is the head
of the Rust view: the replay checkpoint height.

Each vector has the request, Go's answer and how a Rust view with that head
compares:
  "equal"       the Rust answer must equal Go's answer;
  "head"        the request names the head implicitly ("latest", blockNumber);
                Go's answer comes from `go_params`, the same request with the
                explicit head, because the Go node's own head is higher;
  "difference"  a documented difference; `rust` holds the Rust answer and
                `note` says why;
  "derived"     Go answers only for its own head: `go` holds the answer Go
                gives at --head, derived from a Go answer named in `note`.
Only object member order is ignored when comparing. go-quai returns the
access list of quai_createAccessList in Go map order (random); it is sorted
by address and key before it is recorded or compared.

--calls records the call methods only (quai_call, quai_estimateGas,
quai_createAccessList, quai_gasPrice) for a range without contract code:
transfers, new accounts, precompiles and access lists.
"""
import argparse
import json
import re
import urllib.request

# --- keccak-256 and EIP-55 checksums (no third-party packages) -------------
RC = [0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
      0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
      0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
      0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
      0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
      0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008]
ROT = [[0, 36, 3, 41, 18], [1, 44, 10, 45, 2], [62, 6, 43, 15, 61], [28, 55, 25, 21, 56],
       [27, 20, 39, 8, 14]]
M64 = (1 << 64) - 1


def _rol(x, n):
    n %= 64
    return ((x << n) | (x >> (64 - n))) & M64 if n else x


def keccak256(data):
    rate = 136
    p = bytearray(data) + b'\x01'
    while len(p) % rate:
        p.append(0)
    p[-1] |= 0x80
    a = [[0] * 5 for _ in range(5)]
    for off in range(0, len(p), rate):
        for i in range(rate // 8):
            a[i % 5][i // 5] ^= int.from_bytes(p[off + 8 * i:off + 8 * i + 8], 'little')
        for rc in RC:
            c = [a[x][0] ^ a[x][1] ^ a[x][2] ^ a[x][3] ^ a[x][4] for x in range(5)]
            d = [c[(x - 1) % 5] ^ _rol(c[(x + 1) % 5], 1) for x in range(5)]
            a = [[a[x][y] ^ d[x] for y in range(5)] for x in range(5)]
            b = [[0] * 5 for _ in range(5)]
            for x in range(5):
                for y in range(5):
                    b[y][(2 * x + 3 * y) % 5] = _rol(a[x][y], ROT[x][y])
            a = [[b[x][y] ^ ((~b[(x + 1) % 5][y]) & b[(x + 2) % 5][y]) for y in range(5)]
                 for x in range(5)]
            a[0][0] ^= rc
    return b''.join(a[i % 5][i // 5].to_bytes(8, 'little') for i in range(4))


def checksum(address):
    a = address.lower().removeprefix('0x')
    h = keccak256(a.encode()).hex()
    return '0x' + ''.join(c.upper() if c in 'abcdef' and int(h[i], 16) > 7 else c
                          for i, c in enumerate(a))


assert keccak256(b'').hex() == 'c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470'


# --- Go node access ----------------------------------------------------------
READ_ONLY = ('quai_', 'net_version')
WRITE = {'quai_sendRawTransaction', 'quai_submitBlock', 'quai_receiveMinedHeader',
         'quai_receiveWorkShare', 'quai_receiveRawWorkShare', 'quai_setWorkShareP2PThreshold',
         'quai_submitAuxTemplate', 'quai_signAuxTemplate', 'quai_submitShaBlock',
         'quai_submitScryptBlock', 'quai_submitKawpowBlock'}


POINTER = re.compile(r'\b0x[0-9a-f]{6,16}\b')


def sort_access_list(answer):
    """Make two go-quai answers deterministic: createAccessList tuples come in
    Go map order, and "failed to apply transaction" prints a types.Message
    whose pointers change from run to run (each becomes 0xc000000000)."""
    r = answer.get('result')
    if isinstance(r, dict) and isinstance(r.get('accessList'), list):
        for t in r['accessList']:
            t['storageKeys'] = sorted(t['storageKeys'])
        r['accessList'].sort(key=lambda t: t['address'].lower())
    e = answer.get('error')
    if isinstance(e, dict) and str(e.get('message', '')).startswith('failed to apply transaction: {'):
        m = e['message']
        end = m.rfind('} err: ')
        e['message'] = POINTER.sub('0xc000000000', m[:end]) + m[end:]
    return answer


class Go:
    def __init__(self, url):
        self.url = url

    def raw(self, method, params):
        if method in WRITE or not method.startswith(READ_ONLY):
            raise SystemExit(f'refusing to send {method} to the Go node')
        body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params})
        req = urllib.request.Request(self.url, data=body.encode(),
                                     headers={'content-type': 'application/json'})
        answer = json.loads(urllib.request.urlopen(req, timeout=120).read())
        return sort_access_list({k: answer[k] for k in ('result', 'error') if k in answer})

    def result(self, method, params):
        a = self.raw(method, params)
        if 'error' in a:
            raise SystemExit(f'{method} {params}: {a["error"]}')
        return a['result']


def build(go, head):
    vectors = []

    def add(method, params, compare='equal', go_params=None, rust=None, note=None):
        v = {'method': method, 'params': params, 'compare': compare,
             'go': go.raw(method, go_params if go_params is not None else params)}
        if go_params is not None:
            v['go_params'] = go_params
        if rust is not None:
            v['rust'] = rust
        if note:
            v['note'] = note
        vectors.append(v)

    hx = hex
    H = hx(head)
    head_block = go.result('quai_getBlockByNumber', [H, False])
    head_hash = head_block['hash']

    # Blocks: fixed early heights, the head, its parent, and for long views
    # a spread of heights with every order, block uncles and transactions.
    heights = {0, 1, 2, 3, 7, head - 1, head}
    if head > 1000:
        heights |= {12, 22, 93, 195, 1000, 5000, 12345, 20000, 27000, head - 2}
        heights |= set(range(head - 40, head - 2, 9))
    heights = sorted(h for h in heights if 0 <= h <= head)
    txs = []
    for n in heights:
        b = go.result('quai_getBlockByNumber', [hx(n), True])
        h = b['hash']
        add('quai_getBlockByNumber', [hx(n), False])
        add('quai_getBlockByNumber', [hx(n), True])
        add('quai_getBlockByHash', [h, False])
        add('quai_getBlockByHash', [h, True])
        add('quai_getHeaderByNumber', [hx(n)])
        add('quai_getHeaderByHash', [h])
        add('quai_getBlockTransactionCountByNumber', [hx(n)])
        add('quai_getBlockTransactionCountByHash', [h])
        add('quai_getUncleCountByBlockNumber', [hx(n)])
        add('quai_getUncleCountByBlockHash', [h])
        add('quai_getUncleByBlockNumberAndIndex', [hx(n), '0x0'])
        add('quai_getUncleByBlockHashAndIndex', [h, '0x1'])
        # Uncle and workshare hashes: go-quai stores no block for them.
        for u in (b['uncles'][:1] + b['workshares'][:1]):
            add('quai_getBlockByHash', [u['hash'], False])
            add('quai_getHeaderByHash', [u['hash']])
        for i, tx in enumerate(b['transactions'][:3]):
            txs.append((n, h, i, tx['hash']))
        if b['transactions']:
            last = len(b['transactions']) - 1
            txs.append((n, h, last, b['transactions'][last]['hash']))
    for n, h, i, t in txs:
        add('quai_getTransactionByHash', [t])
        add('quai_getTransactionReceipt', [t])
        add('quai_getRawTransactionByHash', [t])
        add('quai_getTransactionByBlockNumberAndIndex', [hx(n), hx(i)])
        add('quai_getTransactionByBlockHashAndIndex', [h, hx(i), [0, 0]])
    if txs:
        n, h, _, _ = txs[0]
        count = int(go.result('quai_getBlockTransactionCountByNumber', [hx(n)]), 16)
        add('quai_getTransactionByBlockNumberAndIndex', [hx(n), hx(count)])
        add('quai_getTransactionByBlockHashAndIndex', [h, '0x0', None])
        add('quai_getTransactionByBlockHashAndIndex', [h, '0x0'])
        add('quai_getTransactionByBlockHashAndIndex', [h, '0x0', '0x0000'])

    # Logs. The export range has no user transactions, so no logs; these
    # check ranges, filters and the error answers.
    start = max(0, head - 9999)
    add('quai_getLogs', [{'fromBlock': hx(start), 'toBlock': H}])
    add('quai_getLogs', [{'fromBlock': '0x0', 'toBlock': hx(min(head, 100))}])
    add('quai_getLogs', [{'blockHash': head_hash}])
    add('quai_getLogs', [{'fromBlock': '0x0', 'toBlock': H,
                          'address': '0x0000024c83970aea9451a6170e4048d5786745ad'}]
        if head <= 10000 else
        [{'fromBlock': hx(start), 'toBlock': H,
          'address': ['0x0000024c83970aea9451a6170e4048d5786745ad']}])
    add('quai_getLogs', [{'fromBlock': hx(max(0, head - 5)), 'toBlock': H,
                          'topics': [None, ['0x' + '11' * 32, None]]}])
    add('quai_getLogs', [{'fromBlock': H, 'toBlock': hx(max(0, head - 1))}])
    add('quai_getLogs', [{'fromBlock': '0x0', 'toBlock': hx(10001)}])
    add('quai_getLogs', [{'blockHash': '0x' + '11' * 32}])
    add('quai_getLogs', [{'blockHash': head_hash, 'fromBlock': '0x0'}])
    add('quai_getLogs', [{'address': '0x12'}])
    add('quai_getLogs', [{'address': [5]}])
    add('quai_getLogs', [{'topics': ['0x12']}])
    add('quai_getLogs', [{'fromBlock': 'zz'}])
    add('quai_getLogs', [])
    add('quai_getLogs', [{'fromBlock': H}], compare='head', go_params=[{'fromBlock': H, 'toBlock': H}])

    # Account state at the head (by number and by hash).
    alloc = ['0x0000024c83970aea9451a6170e4048d5786745ad',
             '0x0000044757587287d5ef86e5020f32913058c6c4',
             '0x0000055114f34a0baa741dd2a77a89638b83a821']
    addresses = [checksum(a) for a in alloc]
    addresses.append(head_block['woHeader']['primaryCoinbase'])
    addresses.append('0x' + '00' * 20)
    qi = checksum('0x0080' + '4c83970aea9451a6170e4048d5786745ad01')
    assert len(qi) == 42
    other = checksum('0x1000024c83970aea9451a6170e4048d5786745ad')
    by_hash = {'blockHash': head_hash}
    for a in addresses:
        for at in (H, by_hash):
            add('quai_getBalance', [a, at])
            add('quai_getTransactionCount', [a, at])
            add('quai_getCode', [a, at])
            add('quai_getStorageAt', [a, '0x0', at])
        add('quai_getBalance', [a, 'latest'], compare='head', go_params=[a, H])
        add('quai_getTransactionCount', [a, 'latest'], compare='head', go_params=[a, H])
    add('quai_getStorageAt', [addresses[0], '0x1zz', H])
    add('quai_getStorageAt', [addresses[0], 5, H])
    add('quai_getBalance', [qi, H],
        compare='equal' if head == go.result('quai_blockNumber', []) else 'difference',
        rust={'result': '0x0'},
        note='Qi balances are served for the head only; the Go head is higher')
    add('quai_getBalance', [qi, 'latest'], compare='head', go_params=[qi, 'latest'])
    add('quai_getTransactionCount', [qi, H])
    add('quai_getBalance', [other, H])
    add('quai_getCode', [other, H])
    add('quai_getStorageAt', [other, '0x0', H])
    add('quai_getBalance', [alloc[0], H])
    add('quai_getBalance', [addresses[0], hx(head + 10 ** 7)])
    add('quai_getBalance', [addresses[0], '0x' + '11' * 32])
    add('quai_getBalance', [addresses[0], {}])
    add('quai_getBalance', [addresses[0], {'blockNumber': H, 'blockHash': head_hash}])
    add('quai_getBalance', [addresses[0], 'pending'])
    add('quai_getBalance', [addresses[0]])
    add('quai_getBalance', [addresses[0], H, 'extra'])
    add('quai_getBalance', {'address': addresses[0]})
    add('quai_getBalance', ['0x12', H])
    add('quai_getBalance', [5, H])
    # State of older blocks: a pruned node keeps only the head tries.
    # state.New opens the EVM trie, then the ETX trie; an empty root needs no
    # node. Blocks whose roots the head shares are served; others fail with
    # the first missing root.
    cur = (head_block['header']['evmRoot'], head_block['header']['etxSetRoot'])
    empty = ('0x' + '00' * 32,
             '0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421')

    def missing(roots):
        for root, have in zip(roots, cur):
            if root not in empty and root != have:
                return root
        return None
    candidates = sorted({head - 1} | {h for h in heights if 0 < h < head - 1}, reverse=True)
    served = found = False
    for n in candidates:
        old = go.result('quai_getBlockByNumber', [hx(n), False])
        root = missing((old['header']['evmRoot'], old['header']['etxSetRoot']))
        if root is None and not served:
            served = True
            add('quai_getBalance', [addresses[0], hx(n)])
            add('quai_getTransactionCount', [addresses[0], {'blockHash': old['hash']}])
        if root is not None and not found:
            found = True
            for method, params in (('quai_getBalance', [addresses[0], hx(n)]),
                                   ('quai_getTransactionCount', [addresses[0], hx(n)]),
                                   ('quai_getCode', [addresses[0], hx(n)]),
                                   ('quai_getStorageAt', [addresses[0], '0x0', hx(n)]),
                                   ('quai_call', [{'to': addresses[1]}, hx(n)]),
                                   ('quai_getBalance', [addresses[0], {'blockHash': old['hash']}])):
                add(method, params, compare='difference',
                    rust={'error': {'code': -32000,
                                    'message': f'missing trie node {root[2:]} (path )'}},
                    note='the Rust view keeps only the head state, as a pruned Go node')
    add('quai_getBalance', [addresses[0], '0x0'])

    # UTXOs. The range has no Qi outputs.
    add('quai_getUTXO', [head_hash, '0x0'])
    add('quai_getUTXO', [head_hash, '0x10000'])
    add('quai_getOutpointsByAddress', [qi], compare='head', go_params=[qi])
    add('quai_getOutpointsByAddress', [addresses[0]])
    add('quai_getOutPointsByAddressAndRange', [qi, '0x0', hx(min(head, 1000))])
    add('quai_getOutPointsByAddressAndRange', [qi, '0x10', '0x1'])
    add('quai_getOutPointsByAddressAndRange', [qi, '0x0', '0x3e9'])
    add('quai_getOutPointsByAddressAndRange', [qi, '0x0', '0x100000000'])
    add('quai_getOutPointsByAddressAndRange', [addresses[0], '0x0', '0x1'])

    # Calls and estimates at the head. There is no contract code in this
    # range, and go-quai v0.57.0 cannot decode state overrides.
    a, b, c = addresses[0], addresses[1], addresses[2]
    add('quai_call', [{'from': a, 'to': b, 'value': '0x1'}, H])
    add('quai_call', [{'from': a, 'to': b, 'value': '0x1'}, by_hash])
    add('quai_call', [{'to': b, 'data': '0x1234'}, H])
    add('quai_call', [{'to': b, 'input': '0x1234', 'data': '0x1234'}, H])
    add('quai_call', [{'from': c, 'to': b, 'value': '0x1'}, H])
    add('quai_call', [{'to': b, 'gas': '0x100'}, H])
    add('quai_call', [{'to': b}, '0x0'])
    add('quai_call', [{'from': qi, 'to': b}, H])
    add('quai_call', [{'from': other, 'to': b}, H])
    add('quai_call', [{'to': b}, H, {b: {'code': '0x00'}}])
    add('quai_call', [{'to': b}, H, {}])
    add('quai_call', [{'from': '0x12', 'to': b}, H])
    add('quai_call', [{'to': b, 'gas': 5}, H])
    add('quai_call', ['x', H])
    add('quai_call', [{'to': b}])
    add('quai_call', [{'to': b}, hx(head + 10 ** 7)])
    add('quai_call', [{'from': a, 'to': b, 'value': '0x1'}, 'latest'], compare='head',
        go_params=[{'from': a, 'to': b, 'value': '0x1'}, H])
    add('quai_estimateGas', [{'from': a, 'to': b, 'value': '0x1'}, H])
    add('quai_estimateGas', [{'from': a, 'to': b, 'data': '0x0011'}, H])
    add('quai_estimateGas', [{'to': b}, H])
    add('quai_estimateGas', [{'from': a, 'to': b, 'gas': '0x5208'}, H])
    add('quai_estimateGas', [{'from': a, 'to': b, 'gas': '0x5000'}, H])
    add('quai_estimateGas', [{'from': c, 'to': b, 'value': '0x1'}, H])
    add('quai_estimateGas', [{'from': a, 'to': b, 'gasPrice': '0x1', 'value': '0x1'}, H])
    add('quai_estimateGas', [{'from': c, 'to': b, 'gasPrice': '0x1', 'value': '0x1'}, H])
    add('quai_estimateGas', [{'from': a, 'to': b, 'txType': 7}, H])
    add('quai_estimateGas', [{'from': a, 'to': b}, '0x0'])
    add('quai_estimateGas', [{'from': a, 'to': b}], compare='head',
        go_params=[{'from': a, 'to': b}, H])

    # Chain identity and head.
    add('quai_chainId', [])
    add('quai_nodeLocation', [])
    add('net_version', [])
    add('quai_chainId', [1])
    add('quai_blockNumber', [], compare='difference', rust={'result': H},
        note='quai_blockNumber is the head of each node')
    add('quai_getBlockByNumber', ['latest', False], compare='head', go_params=[H, False])
    add('quai_getHeaderByNumber', ['latest'], compare='head', go_params=[H])

    # Unknown blocks and transactions, and bad parameters.
    far = hx(0xfffffff)
    add('quai_getBlockByNumber', [far, False])
    add('quai_getHeaderByNumber', [far])
    add('quai_getBlockByHash', ['0x' + '11' * 32, False])
    add('quai_getHeaderByHash', ['0x' + '11' * 32])
    add('quai_getTransactionByHash', ['0x' + '11' * 32])
    add('quai_getTransactionReceipt', ['0x' + '11' * 32])
    add('quai_getRawTransactionByHash', ['0x' + '11' * 32])
    add('quai_getBlockByNumber', ['pending', False])
    add('quai_getBlockByNumber', ['zz', False])
    add('quai_getBlockByNumber', ['0x', False])
    add('quai_getBlockByNumber', ['0x01', False])
    add('quai_getBlockByNumber', [1, False])
    add('quai_getBlockByNumber', ['0x1'])
    add('quai_getBlockByNumber', ['0x1', 'true'])
    add('quai_getBlockByNumber', ['0x1', False, 1])
    add('quai_getBlockByHash', ['0x12', False])
    add('quai_getBlockByHash', ['12', False])
    add('quai_getBlockByHash', [12, False])
    add('quai_getBlockByHash', ['0x' + 'zz' * 32, False])
    add('quai_getTransactionByHash', [])
    add('quai_getTransactionByBlockNumberAndIndex', ['0x1', 1])
    add('quai_getUTXO', [head_hash])
    add('quai_noSuchMethod', [])
    return vectors


def build_calls(go, head):
    """Calls, estimates and access lists at the head, and the gas price."""
    vectors = []
    H = hex(head)

    def add(method, params, compare='equal', go_params=None, note=None):
        v = {'method': method, 'params': params, 'compare': compare,
             'go': go.raw(method, go_params if go_params is not None else params)}
        if go_params is not None:
            v['go_params'] = go_params
        if note:
            v['note'] = note
        vectors.append(v)

    alloc = ['0x0000024c83970aea9451a6170e4048d5786745ad',
             '0x0000044757587287d5ef86e5020f32913058c6c4',
             '0x0000055114f34a0baa741dd2a77a89638b83a821']
    a, b, c = (checksum(x) for x in alloc)
    coinbase = go.result('quai_getBlockByNumber', [H, False])['woHeader']['primaryCoinbase']
    fresh = checksum('0x00001111222233334444555566667777888899aa')
    qi = checksum('0x0080' + '4c83970aea9451a6170e4048d5786745ad01')
    other = checksum('0x1000024c83970aea9451a6170e4048d5786745ad')
    pre = lambda n: '0x' + '00' * 19 + f'{n:02x}'
    ecrec = '0x' + '11' * 32 + '00' * 31 + '1c' + '22' * 32 + '33' * 32
    modexp = '0x' + f'{1:064x}' + f'{1:064x}' + f'{1:064x}' + '03' + '05' + '07'
    bn_add = '0x' + '00' * 128
    bn_bad = '0x' + 'ff' * 64
    pairing_empty = '0x'
    pairing_bad = '0x' + '00' * 100
    blake_bad = '0x' + '00' * 212
    blake_flag = '0x' + '0000000c' + '00' * 208 + '02'
    blake_ok = '0x' + '0000000c' + '00' * 208 + '01'
    requests = [
        {'from': a, 'to': b, 'value': '0x1'},
        {'from': a, 'to': fresh, 'value': '0x1'},
        {'from': a, 'to': fresh},
        {'from': a, 'to': b, 'data': '0x0011'},
        {'from': a, 'to': b, 'value': '0x1', 'accessList': [
            {'address': b, 'storageKeys': []},
            {'address': fresh, 'storageKeys': ['0x' + '00' * 31 + '01']}]},
        {'from': a, 'to': b, 'accessList': [{'address': a, 'storageKeys': []},
                                            {'address': pre(1), 'storageKeys': []}]},
        {'from': a, 'to': coinbase, 'value': '0x2'},
        {'from': b, 'to': a, 'value': '0x10'},
        {'from': c, 'to': a, 'value': '0x1'},
        {'from': a, 'to': a, 'value': '0x1'},
        {'from': a, 'to': b, 'gas': '0x5208', 'value': '0x1'},
        {'from': a, 'to': fresh, 'gas': '0x5208', 'value': '0x1'},
        {'from': a, 'to': fresh, 'gas': '0xa000', 'value': '0x1'},
        {'from': a, 'to': b, 'gas': '0x5200'},
        {'from': a, 'to': b, 'gasPrice': '0x3b9aca00', 'value': '0x1'},
        {'from': a, 'data': '0x600060005360016000f3'},
        {'from': a, 'to': other, 'value': '0x1'},
        {'from': a, 'to': qi, 'value': '0x1'},
        {'to': b},
        {'from': a, 'to': pre(1), 'data': ecrec},
        {'from': a, 'to': pre(1), 'data': '0x1234'},
        {'from': a, 'to': pre(2), 'data': '0x68656c6c6f'},
        {'from': a, 'to': pre(3), 'data': '0x68656c6c6f'},
        {'from': a, 'to': pre(4), 'data': '0x68656c6c6f'},
        {'from': a, 'to': pre(5), 'data': modexp},
        {'from': a, 'to': pre(6), 'data': bn_add},
        {'from': a, 'to': pre(6), 'data': bn_bad},
        {'from': a, 'to': pre(7), 'data': bn_bad},
        {'from': a, 'to': pre(8), 'data': pairing_empty},
        {'from': a, 'to': pre(8), 'data': pairing_bad},
        {'from': a, 'to': pre(9), 'data': blake_bad},
        {'from': a, 'to': pre(9), 'data': blake_flag},
        {'from': a, 'to': pre(9), 'data': blake_ok},
        {'from': a, 'to': pre(2), 'data': '0x68656c6c6f', 'gas': '0x5300'},
        {'from': a, 'to': '0x000000000000000000000000000000000000000a', 'data': '0x'},
    ]
    for method in ('quai_call', 'quai_estimateGas', 'quai_createAccessList'):
        for r in requests:
            if method == 'quai_estimateGas' and r.get('to') == qi:
                continue  # a conversion estimate: not served natively
            add(method, [r, H])
        add(method, [{'from': a, 'to': b, 'value': '0x1'}, 'latest'], compare='head',
            go_params=[{'from': a, 'to': b, 'value': '0x1'}, H])
    add('quai_estimateGas', [{'from': a, 'to': pre(2), 'data': '0x01'}], compare='head',
        go_params=[{'from': a, 'to': pre(2), 'data': '0x01'}, H])
    add('quai_createAccessList', [{'from': a, 'to': pre(4), 'data': '0x01'}], compare='head',
        go_params=[{'from': a, 'to': pre(4), 'data': '0x01'}, H])
    # quai_gasPrice is CalcBaseFee(head) * 120 / 100, and header validation
    # makes CalcBaseFee(head) the base fee of block head + 1.
    child = go.result('quai_getBlockByNumber', [hex(head + 1), False])
    fee = int(child['header']['baseFeePerGas'], 16)
    vectors.append({'method': 'quai_gasPrice', 'params': [], 'compare': 'derived',
                    'go': {'result': hex(fee * 120 // 100)},
                    'note': f'baseFeePerGas of block {head + 1} * 120 / 100 '
                            '(quai_getBlockByNumber of the Go node)'})
    add('quai_gasPrice', [1])
    return vectors


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--go', default='http://127.0.0.1:9200')
    ap.add_argument('--head', type=int, required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('--calls', action='store_true',
                    help='record only the call methods (quai_call, quai_estimateGas, '
                         'quai_createAccessList, quai_gasPrice)')
    args = ap.parse_args()
    go = Go(args.go)
    if go.result('quai_chainId', []) != '0x9':
        raise SystemExit('not a mainnet node')
    if int(go.result('quai_blockNumber', []), 16) < args.head + (1 if args.calls else 0):
        raise SystemExit('the Go node is below --head')
    vectors = build_calls(go, args.head) if args.calls else build(go, args.head)
    doc = {
        'source': 'go-quai v0.57.0 (33db82f2891b267ba898ca38bd2dd8bf75cbb072), zone [0 0], '
                  'HTTP JSON-RPC, RPC version v1',
        'generator': 'scripts/gen_native_rpc_vectors.py',
        'head': args.head,
        'head_hash': go.result('quai_getBlockByNumber', [hex(args.head), False])['hash'],
        'vectors': vectors,
    }
    with open(args.out, 'w') as f:
        json.dump(doc, f, indent=1, sort_keys=True)
        f.write('\n')
    counts = {}
    for v in vectors:
        counts[v['compare']] = counts.get(v['compare'], 0) + 1
    print(f'{args.out}: {len(vectors)} vectors {counts}')


if __name__ == '__main__':
    main()
