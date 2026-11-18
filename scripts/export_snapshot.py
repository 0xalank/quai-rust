"""Capture bounded, consistent local-node reads into a checksummed materialized view."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import tempfile
from urllib.parse import urlparse

from fullnode import call


def payload_bytes(payload):
    return json.dumps(payload, separators=(',', ':'), ensure_ascii=False, sort_keys=True).encode()


def seal(payload):
    raw = payload_bytes(payload)
    digest = hashlib.sha256(raw).hexdigest()
    return b'{"sha256":"' + digest.encode() + b'","payload":' + raw + b'}\n'


def unseal(path):
    text = Path(path).read_text()
    envelope = json.loads(text)
    decoder = json.JSONDecoder()
    cursor = text.index('{') + 1
    raw_payload = None
    seen = set()
    while True:
        while text[cursor].isspace():
            cursor += 1
        if text[cursor] == '}':
            break
        key, cursor = decoder.raw_decode(text, cursor)
        if key in seen:
            raise ValueError('Duplicate export envelope field')
        seen.add(key)
        while text[cursor].isspace():
            cursor += 1
        if text[cursor] != ':':
            raise ValueError('Invalid envelope')
        cursor += 1
        while text[cursor].isspace():
            cursor += 1
        start = cursor
        _, cursor = decoder.raw_decode(text, cursor)
        if key == 'payload':
            raw_payload = text[start:cursor].encode()
        while text[cursor].isspace():
            cursor += 1
        if text[cursor] == ',':
            cursor += 1
        elif text[cursor] != '}':
            raise ValueError('Invalid envelope')
    if raw_payload is None or hashlib.sha256(raw_payload).hexdigest() != envelope['sha256']:
        raise ValueError('Export SHA-256 mismatch')
    return envelope['payload']


def atomic_write(path, data):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=path.name + '.', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as file:
            file.write(data)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def capture(url, max_blocks=1024, start=None):
    parsed = urlparse(url)
    if parsed.scheme != 'http' or parsed.hostname not in ('127.0.0.1', 'localhost', '::1') or parsed.username:
        raise ValueError('Capture requires a loopback HTTP endpoint without credentials')
    chain_id = call(url, 'quai_chainId')
    location = call(url, 'quai_nodeLocation')
    genesis = call(url, 'quai_getBlockByNumber', ['0x0', True])
    head = call(url, 'quai_getBlockByNumber', ['latest', True])
    height = int(head['woHeader']['number'], 16)
    if not 1 <= max_blocks <= 100000:
        raise ValueError('Use 1–100000 blocks per export')
    start = max(0, height - max_blocks + 1) if start is None else start
    if start < 0 or start > height or height - start + 1 > max_blocks:
        raise ValueError('Invalid or oversized export range')
    anchor = None
    if start:
        parent = call(url, 'quai_getBlockByNumber', [hex(start - 1), False])
        anchor = {'number': start - 1, 'hash': parent['hash']}
    blocks, receipts, canonical = [], [], []
    for number in range(start, height + 1):
        block = call(url, 'quai_getBlockByNumber', [hex(number), True])
        if int(block['woHeader']['number'], 16) != number:
            raise ValueError('Reference returned the wrong height')
        if (canonical and block['woHeader']['parentHash'] != canonical[-1]) or (not canonical and anchor and block['woHeader']['parentHash'] != anchor['hash']):
            raise ValueError('Reference chain changed during capture')
        blocks.append(block)
        canonical.append(block['hash'])
        for tx in block['transactions']:
            receipt = call(url, 'quai_getTransactionReceipt', [tx['hash']])
            if not receipt or receipt['blockHash'] != block['hash']:
                raise ValueError('Missing or inconsistent receipt during capture')
            receipts.append(receipt)
    after = call(url, 'quai_getBlockByNumber', [hex(height), True])
    if after['hash'] != head['hash'] or canonical[-1] != head['hash']:
        raise ValueError('Reference head changed; discard capture and retry against stable state')
    return {'schema': 1, 'identity': {'chain_id': chain_id,
                                    'genesis_hash': genesis['hash'],
                                    'location': [int(v, 16) for v in location]},
            'base_number': start, 'anchor': anchor, 'canonical': canonical, 'blocks': blocks, 'receipts': receipts,
            'receipts_complete': True,
            'source': {'captured_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                       'head_hash': head['hash'], 'trust': 'RPC export; consensus remains the source node responsibility'}}


def merge(previous, current):
    if previous['identity'] != current['identity']:
        raise ValueError('Cannot merge exports from different networks')
    result = dict(current)
    blocks, receipts = {}, {}
    for snapshot in [previous, current]:
        for block in snapshot['blocks']:
            key = block['hash']
            if key in blocks and blocks[key] != block:
                raise ValueError('Immutable block content changed')
            blocks[key] = block
        for receipt in snapshot['receipts']:
            key = (receipt['blockHash'], receipt['transactionHash'])
            if key in receipts and receipts[key] != receipt:
                raise ValueError('Immutable receipt content changed')
            receipts[key] = receipt
    result['blocks'], result['receipts'] = list(blocks.values()), list(receipts.values())
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('--max-blocks', type=int, default=1024)
    parser.add_argument('--merge')
    parser.add_argument('--start-block', type=int)
    args = parser.parse_args()
    view = capture(args.url, args.max_blocks, args.start_block)
    if args.merge:
        view = merge(unseal(args.merge), view)
    atomic_write(args.out, seal(view))
    print(json.dumps({'blocks': len(view['blocks']), 'receipts': len(view['receipts']),
                      'head': view['canonical'][-1], 'out': args.out}))
