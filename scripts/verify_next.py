#!/usr/bin/env python3
"""Full-node, SDK and native read-store acceptance; all services are local."""
import datetime
import json
from pathlib import Path
import signal
import tempfile
import time

from compat_rpc import compare, decode, exchange, get_json, select_scenario
from export_snapshot import atomic_write, capture, merge, seal
from fullnode import FullNode, call
from lab import ROOT, Server, save, sha
from next_lab import scenario, sdk_compare


def request(method, params, id='integration'):
    return json.dumps({'jsonrpc': '2.0', 'id': id, 'method': method, 'params': params})


def parity(reference, candidate, method, params, trace):
    raw = request(method, params)
    expected, actual = exchange(reference, raw), exchange(candidate, raw)
    compare(raw, expected, actual)
    trace.append({'method': method, 'request': raw, 'response': expected})


def await_health(server, predicate):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        health = get_json(server.url + '/health')
        if predicate(health):
            return health
        time.sleep(.05)
    raise AssertionError('Native store transition did not complete')


def main():
    build = json.loads((ROOT / 'results/build.json').read_text())
    for name, digest in build['binaries'].items():
        assert sha(ROOT / 'bin' / name) == digest, 'Rebuild changed executable: ' + name
    for name, digest in build['rust_sources'].items():
        assert sha(ROOT / name) == digest, 'Rebuild changed Rust source: ' + name
    run = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    out = ROOT / 'results' / ('acceptance-' + run)
    out.mkdir(parents=True)
    trace, sdk, checks = [], [], []
    with FullNode() as node:
        print('full node ready', flush=True)
        view = capture(node.url)
        export_path = ROOT / 'fixtures/native/fullnode-genesis.json'
        atomic_write(export_path, seal(view))
        save(out / 'fullnode-profile.json', node.profile)
        block_hash = view['canonical'][0]
        with Server(extra={'mode': 'gateway', 'upstream': node.url}) as gateway:
            for method, params in [
                ('quai_chainId', []), ('quai_nodeLocation', []), ('quai_listRunningChains', []),
                ('quai_blockNumber', []), ('quai_getBlockByNumber', ['0x0', True]),
                ('quai_getBlockByHash', [block_hash, False]),
                ('quai_getLogs', [{'blockHash': block_hash}]),
                ('quai_getTransactionReceipt', ['0x' + '00' * 32]),
                ('quai_getBlockByHash', ['0x' + '11' * 32, True]),
                ('quai_getBalance', ['0x0010000000000000000000000000000000000001', 'latest']),
                ('quai_getBalance', ['0x01', 'latest']),
            ]:
                parity(node.url, gateway.url, method, params, trace)
            sdk += sdk_compare(node.url, gateway.url, node.profile, 'fullnode-gateway', out)
        with Server(extra={'mode': 'hybrid', 'upstream': node.url, 'snapshot': str(export_path)}) as hybrid:
            parity(node.url, hybrid.url, 'quai_getBlockByHash', [block_hash, True], trace)
            sdk += sdk_compare(node.url, hybrid.url, node.profile, 'fullnode-hybrid', out)
            before = get_json(hybrid.url + '/health')
            node.stop()
            offline = call(hybrid.url, 'quai_getBlockByHash', [block_hash, True])
            assert offline == view['blocks'][0]
            unavailable = decode(exchange(hybrid.url, request('quai_getTransactionReceipt', ['0x' + '00' * 32]))['body'])
            assert unavailable['error']['code'] == -32098
            checks.append('hybrid immutable read survives source outage; live receipt does not use stale snapshot')
            node.start()
            assert node.profile['genesis_hash'] == view['identity']['genesis_hash']
            parity(node.url, hybrid.url, 'quai_getBlockByHash', [block_hash, False], trace)
            assert get_json(hybrid.url + '/health')['native_hits'] > before['native_hits']
        checks.append('full hierarchy restarts against the same on-disk local genesis')
    with scenario() as (url, profile, _):
        print('typed block scenarios ready', flush=True)
        a = capture(url)
        atomic_write(ROOT / 'fixtures/native/scenario-a.json', seal(a))
        with Server(extra={'mode': 'gateway', 'upstream': url}) as gateway:
            sdk += sdk_compare(url, gateway.url, profile, 'funded-scenarios', out)
            for block in a['blocks']:
                for full in [False, True]:
                    parity(url, gateway.url, 'quai_getBlockByHash', [block['hash'], full], trace)
            for receipt in a['receipts']:
                parity(url, gateway.url, 'quai_getTransactionReceipt', [receipt['transactionHash']], trace)
            parity(url, gateway.url, 'quai_getLogs', [{'blockHash': a['canonical'][-1]}], trace)
        b_profile = select_scenario(url, 'alternate')
        b = merge(a, capture(url))
        atomic_write(ROOT / 'fixtures/native/scenario-b.json', seal(b))
        select_scenario(url, 'canonical')
        with tempfile.TemporaryDirectory(prefix='quai-rust-store-') as directory:
            current = Path(directory) / 'current.json'
            atomic_write(current, seal(a))
            with Server(extra={'mode': 'snapshot', 'snapshot': str(current)}) as native:
                sdk += sdk_compare(url, native.url, profile, 'native-snapshot', out, subset='store')
                for block in a['blocks']:
                    for full in [False, True]:
                        parity(url, native.url, 'quai_getBlockByHash', [block['hash'], full], trace)
                for receipt in a['receipts']:
                    parity(url, native.url, 'quai_getTransactionReceipt', [receipt['transactionHash']], trace)
                health = get_json(native.url + '/health')
                assert health['upstream_calls'] == 0 and health['native_hits'] > 0
                select_scenario(url, 'alternate')
                atomic_write(current, seal(b))
                native.process.send_signal(signal.SIGHUP)
                await_health(native, lambda h: h['store']['generation'] == 2)
                parity(url, native.url, 'quai_getBlockByNumber', ['latest', True], trace)
                for receipt in a['receipts'] + b['receipts']:
                    parity(url, native.url, 'quai_getTransactionReceipt', [receipt['transactionHash']], trace)
                parity(url, native.url, 'quai_getBlockByHash', [a['canonical'][-1], True], trace)
                checks.append('canonical branch replacement removes orphan receipts and retains immutable blocks')
                atomic_write(current, seal(b).replace(b'0x539', b'0x538', 1))
                native.process.send_signal(signal.SIGHUP)
                await_health(native, lambda h: h['reload_failures'] == 1)
                assert get_json(native.url + '/health')['store']['generation'] == 2
                parity(url, native.url, 'quai_getBlockByNumber', ['latest', True], trace)
                checks.append('checksum corruption rejected without replacing the previous valid view')
                wrong = json.loads(json.dumps(b))
                wrong['identity']['chain_id'] = '0x538'
                atomic_write(current, seal(wrong))
                native.process.send_signal(signal.SIGHUP)
                await_health(native, lambda h: h['reload_failures'] == 2)
                assert get_json(native.url + '/health')['store']['generation'] == 2
                checks.append('validly encoded exports from a different network are rejected')
                # Simulate an interrupted writer; the reader retains its complete view.
                atomic_write(current, seal(b)[:100])
                native.process.send_signal(signal.SIGHUP)
                await_health(native, lambda h: h['reload_failures'] == 3)
                assert get_json(native.url + '/health')['store']['generation'] == 2
                checks.append('truncated import cannot publish a partial canonical view')
            atomic_write(current, seal(b))
            with Server(extra={'mode': 'snapshot', 'snapshot': str(current)}) as restarted:
                parity(url, restarted.url, 'quai_getBlockByNumber', ['latest', True], trace)
                assert get_json(restarted.url + '/health')['upstream_calls'] == 0
            checks.append('native store restart retains the persisted canonical branch')
        partial = capture(url, max_blocks=1)
        assert partial['base_number'] == 2 and partial['anchor']['number'] == 1
        with tempfile.TemporaryDirectory(prefix='quai-rust-partial-') as directory:
            path = Path(directory) / 'partial.json'
            atomic_write(path, seal(partial))
            with Server(extra={'mode': 'snapshot', 'snapshot': str(path)}) as partial_server:
                parity(url, partial_server.url, 'quai_getBlockByHash', [partial['canonical'][-1], True], trace)
                parity(url, partial_server.url, 'quai_getTransactionReceipt', [partial['receipts'][0]['transactionHash']], trace)
                old_receipt = a['receipts'][0]['transactionHash']
                unavailable = decode(exchange(partial_server.url, request('quai_getTransactionReceipt', [old_receipt]))['body'])
                assert unavailable['error']['code'] == -32004
                assert not get_json(partial_server.url + '/health')['store']['history_complete']
        checks.append('anchored partial import serves covered reads and reports uncovered receipts as unavailable')
        # Native and gateway log queries use the Go filter backend; compare only
        # single-block responses strictly because Go range collection is concurrent.
        logs = call(url, 'quai_getLogs', [{'fromBlock': '0x1', 'toBlock': '0x2'}])
        assert sorted(v['data'] for v in logs) == ['0x07', '0x63']
        checks.append('Go range filter returns the selected branch logs, preserving multiplicity')
    failures = [failure for group in sdk for failure in group['failures']]
    result = {'passed': not failures, 'run': run, 'strict_rpc_comparisons': len(trace),
              'sdk_cases': sum(g['cases'] for g in sdk), 'sdk_groups': sdk,
              'scenarios': checks, 'failures': failures,
              'build_sha256': sha(ROOT / 'results/build.json'),
              'sdk_lock_sha256': sha(ROOT / 'sdk/package-lock.json'),
              'harness_source_hashes': {name: sha(ROOT / name) for name in ['scripts/verify_next.py','scripts/fullnode.py','scripts/next_lab.py','scripts/export_snapshot.py','sdk/acceptance.cjs']},
              'scope': 'Real local full-node genesis/restart; funded and typed-block scenarios use seeded Go APIs. No mined/synchronized non-genesis chain or consensus replay claim.'}
    save(out / 'trace.json', trace)
    save(out / 'summary.json', result)
    save(ROOT / 'results/latest-acceptance.json', {'directory': str(out.relative_to(ROOT)), 'passed': result['passed']})
    print(json.dumps({key: result[key] for key in ['passed', 'run', 'strict_rpc_comparisons', 'sdk_cases', 'scenarios', 'failures']}, indent=2))
    if failures:
        raise SystemExit('SDK acceptance failed; inspect the recorded case reports')


if __name__ == '__main__':
    main()
