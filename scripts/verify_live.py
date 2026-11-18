#!/usr/bin/env python3
"""Local mined-chain and fault-injected follower acceptance. No timing claim."""
import contextlib
import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading
import time

from compat_rpc import compare, decode, exchange, get_json, select_scenario
from export_snapshot import unseal
from fullnode import FullNode, call
from lab import ROOT, Server, save, sha
from next_lab import scenario, sdk_compare


def await_state(server, predicate, seconds=15):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        last = get_json(server.url + '/health')
        if predicate(last):
            return last
        time.sleep(.05)
    raise AssertionError(f'Follower transition timed out: {last}')


def followed(head):
    return lambda h: h['ingestion'].get('network_verified') and h['store']['head_hash'] == head


def mine(node, blocks=1, send=False):
    args = [str(ROOT / 'bin/quai-local-mine'), '--url', node.url,
            '--genesis', node.profile['genesis_hash'], '--blocks', str(blocks)]
    if send:
        args += ['--send']
    result = subprocess.run(args, text=True, capture_output=True, timeout=100)
    if result.returncode:
        raise AssertionError(result.stderr + result.stdout + (node.directory / 'process.log').read_text()[-3000:])
    return [json.loads(line) for line in result.stdout.splitlines()]


class FaultProxy:
    def __init__(self, upstream):
        self.upstream, self.mode = upstream, None
        self.calls = []

    def __enter__(self):
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                raw = self.rfile.read(int(self.headers['Content-Length']))
                request = json.loads(raw)
                owner.calls.append(request)
                mode = owner.mode
                method = request['method']
                if mode == 'offline':
                    self.send_error(503)
                    return
                response = exchange(owner.upstream, raw.decode())
                body = response['body'].encode() if isinstance(response['body'], str) else response['body']
                value = json.loads(body)
                if mode == 'identity' and method == 'quai_chainId':
                    value['result'] = '0x538'
                if mode == 'receipt' and method == 'quai_getTransactionReceipt':
                    value['result'] = None
                if mode == 'immutable' and method == 'quai_getBlockByNumber' and request['params'][0] == 'latest':
                    value['result']['changed_immutable_field'] = True
                if mode == 'parent' and method == 'quai_getBlockByNumber' and request['params'][0] == 'latest':
                    value['result']['woHeader']['parentHash'] = '0x' + 'ee' * 32
                if mode == 'race' and method == 'quai_getTransactionReceipt':
                    select_scenario(owner.upstream, 'canonical')
                    owner.mode = None
                body = json.dumps(value).encode()
                if mode == 'json' and method == 'quai_getBlockByNumber':
                    body = b'{'
                if mode == 'oversized' and method == 'quai_getBlockByNumber':
                    body = b' ' * (11 * 1024 * 1024)
                self.send_response(response['status'])
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass
            def log_message(self, *_):
                pass
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


def main():
    run = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    out = ROOT / 'results' / ('live-' + run)
    out.mkdir(parents=True)
    class Checks(list):
        def append(self, item):
            super().append(item)
            print(item, flush=True)
    checks, trace, mining, sdk = Checks(), [], [], []

    def parity(reference, candidate, method, params):
        raw = json.dumps({'jsonrpc':'2.0','id':'live','method':method,'params':params})
        expected, actual = exchange(reference, raw), exchange(candidate, raw)
        compare(raw, expected, actual)
        trace.append({'request':json.loads(raw),'reference':expected,'candidate':actual})

    with tempfile.TemporaryDirectory(prefix='quai-rust-live-acceptance-') as directory:
        temporary = Path(directory)
        # Standard local-network mining uses the unchanged full-node executable.
        with FullNode(consensus='blake3') as node:
            mining += mine(node, blocks=3)
            assert call(node.url,'quai_blockNumber') == '0x3'
            save(out/'standard-node-profile.json',node.profile)
            checks.append('unmodified Go CLI accepts three local Blake3 PoW blocks')

        checkpoint = temporary / 'live.json'
        with FullNode(consensus='blake3', funded=True) as node:
            save(out/'funded-node-profile.json',node.profile)
            mining += mine(node,blocks=2)
            node.stop()
            branch = temporary / 'branch-base'
            shutil.copytree(node.directory/'data', branch)
            node.start()
            options = {'mode':'hybrid','upstream':node.url,'snapshot':str(checkpoint),
                       'follow':'true','poll_ms':100,'window_blocks':3}
            with Server(extra=options) as live:
                first = call(node.url,'quai_getBlockByNumber',['latest',True])
                await_state(live,followed(first['hash']))
                checks.append('empty checkpoint bootstraps automatically from actual Go RPC')
                mining += mine(node,blocks=2,send=True)
                receipt = next(v['mined_transfer_receipt'] for v in mining if 'mined_transfer_receipt' in v)
                tx_hash = receipt['transactionHash']
                head_a = call(node.url,'quai_getBlockByNumber',['latest',True])
                before = await_state(live,followed(head_a['hash']))
                assert before['store']['first_indexed_number'] == 2
                assert before['store']['blocks'] <= 6
                assert unseal(checkpoint)['canonical'][-1] == head_a['hash']
                for full in [False,True]:
                    parity(node.url,live.url,'quai_getBlockByHash',[head_a['hash'],full])
                parity(node.url,live.url,'quai_getTransactionReceipt',[tx_hash])
                parity(node.url,live.url,'quai_getBalance',[node.fixture['account'],'latest'])
                assert call(node.url,'quai_getBalance',[receipt['to'],'latest']) == '0x7b'
                assert call(node.url,'quai_getTransactionCount',[node.fixture['account'],'latest']) == '0x1'
                parity(node.url,live.url,'quai_getBalance',[receipt['to'],'latest'])
                parity(node.url,live.url,'quai_getTransactionCount',[node.fixture['account'],'latest'])
                with Server(extra={'mode':'snapshot','snapshot':str(checkpoint)}) as frozen:
                    parity(node.url,frozen.url,'quai_getTransactionReceipt',[tx_hash])
                    parity(node.url,frozen.url,'quai_getBlockByNumber',['latest',True])
                sdk_profile={**node.profile,'head_hash':head_a['hash'],'receipt_hashes':[tx_hash]}
                sdk += sdk_compare(node.url,live.url,sdk_profile,'mined-live',out,subset='store')
                assert not any(group['failures'] for group in sdk)
                save(out/'checkpoint-branch-a.json',json.loads(checkpoint.read_text()))
                checks.append('signed local Quai transfer is mined and its block and receipt match Rust')
                checks.append('rolling window advances with parent anchor and bounded retained blocks')

                # Restore only this test node's stopped, temporary database.
                # This exercises a real validated replacement branch, not P2P fork choice.
                node.stop()
                offline = await_state(live,lambda h:h['ingestion']['state']=='degraded')
                assert offline['ingestion']['lag_blocks'] is None
                assert call(live.url,'quai_getBlockByHash',[head_a['hash'],True]) == head_a
                live_error = decode(exchange(live.url,json.dumps({'jsonrpc':'2.0','id':1,'method':'quai_getTransactionReceipt','params':[tx_hash]}))['body'])
                assert live_error['error']['code'] == -32098
                checks.append('source outage retains immutable reads and marks live freshness unknown')
                shutil.rmtree(node.directory/'data')
                shutil.copytree(branch,node.directory/'data')
                node.start()
                mining += mine(node,blocks=3)
                head_b = call(node.url,'quai_getBlockByNumber',['latest',True])
                after = await_state(live,followed(head_b['hash']))
                assert after['ingestion']['reorgs'] >= 1
                assert head_a['hash'] != head_b['hash']
                assert call(node.url,'quai_getBalance',[receipt['to'],'latest']) == '0x0'
                assert call(node.url,'quai_getTransactionCount',[node.fixture['account'],'latest']) == '0x0'
                assert unseal(checkpoint)['canonical'][-1] == head_b['hash']
                with Server(extra={'mode':'snapshot','snapshot':str(checkpoint)}) as frozen:
                    missing = decode(exchange(frozen.url,json.dumps({'jsonrpc':'2.0','id':1,'method':'quai_getTransactionReceipt','params':[tx_hash]}))['body'])
                    assert missing['error']['code'] == -32004
                    assert call(node.url,'quai_getTransactionReceipt',[tx_hash]) is None
                    parity(node.url,frozen.url,'quai_getBlockByNumber',['latest',True])
                save(out/'checkpoint-branch-b.json',json.loads(checkpoint.read_text()))
                checks.append('restored Go database produces replacement branch and canonical receipt index rolls back')
                # Crash the writer and restart without any checkpoint repair.
                live.process.kill()
                live.process.wait(timeout=5)
            checkpoint.with_suffix('.pending').write_bytes(b'{"incomplete":')
            with Server(extra=options) as restarted:
                await_state(restarted,followed(head_b['hash']))
                parity(node.url,restarted.url,'quai_getBlockByHash',[head_b['hash'],True])
                checks.append('SIGKILL releases writer lock; committed checkpoint restarts and ignores abandoned pending file')
                node.restart()
                assert call(node.url,'quai_getBlockByNumber',['latest',True])['hash'] == head_b['hash']
                await_state(restarted,followed(head_b['hash']))
                checks.append('non-genesis Go database and follower both survive restart')

        # Fault cases use actual production Go APIs with seeded typed transactions.
        fault_path = temporary/'fault.json'
        with scenario() as (url,profile,_), FaultProxy(url) as proxy:
            options = {'mode':'hybrid','upstream':proxy.url,'snapshot':str(fault_path),
                       'follow':'true','poll_ms':100,'window_blocks':3}
            with Server(extra=options) as live:
                head_a = call(url,'quai_getBlockByNumber',['latest',True])
                await_state(live,followed(head_a['hash']))
                pinned = fault_path.read_bytes()
                duplicate = subprocess.run([str(ROOT/'bin/quai-rpc'),'--mode','hybrid','--follow','true',
                    '--upstream',proxy.url,'--snapshot',str(fault_path),'--listen','127.0.0.1:0'],capture_output=True,text=True,timeout=5)
                assert duplicate.returncode and 'owned by another follower' in duplicate.stderr
                checks.append('second writer is rejected by an OS-backed checkpoint lock')
                live.process.send_signal(signal.SIGHUP)
                time.sleep(.1)
                assert live.process.poll() is None
                assert fault_path.read_bytes() == pinned
                checks.append('manual reload signal is ignored while follower owns checkpoint')
                errors=get_json(live.url+'/health')['ingestion']['errors']
                proxy.mode='immutable'
                await_state(live,lambda h:h['ingestion']['errors']>errors)
                assert fault_path.read_bytes()==pinned
                proxy.mode=None
                await_state(live,followed(head_a['hash']))
                checks.append('changed immutable content at the same head is rejected')
                for fault in ['identity','receipt','parent','json','oversized','race']:
                    previous_errors=get_json(live.url+'/health')['ingestion']['errors']
                    proxy.mode = fault
                    select_scenario(url,'alternate')
                    await_state(live,lambda h:h['ingestion']['errors']>previous_errors)
                    assert fault_path.read_bytes() == pinned, fault
                    select_scenario(url,'canonical')
                    proxy.mode = None
                    await_state(live,followed(head_a['hash']))
                    checks.append(f'{fault} fault rejects candidate without replacing the committed view')
                select_scenario(url,'alternate')
                head_b = call(url,'quai_getBlockByNumber',['latest',True])
                health = await_state(live,followed(head_b['hash']))
                assert health['ingestion']['reorgs'] >= 1
                for full in [False,True]:
                    parity(url,live.url,'quai_getBlockByHash',[head_b['hash'],full])
                checks.append('typed Quai Qi and ETX replacement branch ingests after fault recovery')
                proxy.mode = 'offline'
                await_state(live,lambda h:h['ingestion']['state']=='degraded')
                live.process.kill();live.process.wait(timeout=5)
            with Server(extra=options) as restarted:
                health=await_state(restarted,lambda h:h['ingestion']['state']=='degraded')
                assert health['store']['head_hash']==head_b['hash']
                assert call(restarted.url,'quai_getBlockByHash',[head_b['hash'],True])==head_b
                proxy.mode=None
                await_state(restarted,followed(head_b['hash']))
                checks.append('existing checkpoint starts through upstream outage and catches up on recovery')
            save(out/'typed-checkpoint.json',json.loads(fault_path.read_text()))
            save(out/'fault-proxy-requests.json',proxy.calls)

    result={'passed':True,'run':run,'checks':checks,'strict_rpc_comparisons':len(trace),'sdk_cases':sum(group['cases'] for group in sdk),'sdk_groups':sdk,'mining':mining,
            'source_hashes':{str(p.relative_to(ROOT)):sha(p) for directory in ['crates','scripts','reference'] for p in (ROOT/directory).rglob('*') if p.is_file() and p.suffix in ['.rs','.py','.go']},
            'binary_hashes':{p:sha(ROOT/'bin'/p) for p in ['quai-rpc','go-quai','go-quai-local-fixture','quai-local-mine','quai-scenario-oracle']},
            'scope':'Mined local Blake3 blocks and one signed Quai transfer. Funded fixture uses custom allocations and TimeToStartTx=0; reorg restores this test node database. Typed Qi/ETX fault cases remain seeded API scenarios. No P2P fork-choice or production qualification claim.'}
    save(out/'trace.json',trace)
    save(out/'summary.json',result)
    save(ROOT/'results/latest-live.json',{'directory':str(out.relative_to(ROOT)),'passed':True})
    print(json.dumps({'passed':True,'checks':len(checks),'strict_rpc_comparisons':len(trace),'sdk_cases':sum(group['cases'] for group in sdk),'directory':str(out)},indent=2))


if __name__=='__main__':
    main()
