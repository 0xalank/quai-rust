#!/usr/bin/env python3
"""Strict canonical receipt/log parity, routing witnesses and controlled races."""
import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time

from compat_rpc import compare, decode, exchange, get_json, select_scenario
from export_snapshot import atomic_write, capture, seal
from fullnode import FullNode, call
from lab import ROOT, Server, save, sha
from next_lab import scenario
from verify_live import await_state, followed, mine


class Proxy:
    def __init__(self,url):
        self.source=url;self.mode=None;self.proofs=0;self.switch_at=None;self.witness=[]
        self.gate=threading.Event();self.gate.set()
    def __enter__(self):
        owner=self
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                proof=isinstance(request,list)
                if isinstance(request,dict) and request.get('id')=='ingest':owner.gate.wait(10)
                if owner.mode=='offline' or (proof and owner.mode=='proof-offline'):
                    self.send_error(503);return
                if proof:
                    owner.proofs+=1
                    if owner.switch_at==owner.proofs:select_scenario(owner.source,'alternate')
                response=exchange(owner.source,json.dumps(request))
                value=json.loads(response['body'])
                if proof:
                    if owner.mode=='identity':
                        next(v for v in value if v['id']=='chain')['result']='0x538'
                    if owner.mode=='malformed':value[1]['id']=value[0]['id']
                    if owner.mode=='reverse':value.reverse()
                owner.witness.append({'request':request,'response':{'status':response['status'],'body':json.dumps(value)}})
                body=json.dumps(value).encode()
                self.send_response(response['status']);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers()
                try:self.wfile.write(body)
                except (BrokenPipeError,ConnectionResetError):pass
            def log_message(self,*_):pass
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
        self.url=f'http://127.0.0.1:{self.server.server_port}'
        return self
    def __exit__(self,*_):
        self.gate.set();self.server.shutdown();self.server.server_close();self.thread.join(3)


def main():
    run=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    out=ROOT/'results'/('fresh-'+run);out.mkdir(parents=True)
    trace=[];checks=[];sdk=[]
    def check(label):checks.append(label);print(label,flush=True)
    def request(url,method,params):
        raw=json.dumps({'jsonrpc':'2.0','id':'fresh-test','method':method,'params':params})
        result=exchange(url,raw);trace.append({'request':json.loads(raw),'candidate':result});return raw,result
    def parity(source,candidate,method,params):
        raw,result=request(candidate,method,params)
        expected=exchange(source,raw);compare(raw,expected,result);trace[-1]['reference']=expected
        return decode(result['body']).get('result')
    def witnessed(proxy,candidate,method,params):
        mark=len(proxy.witness);raw,result=request(candidate,method,params)
        matches=[v for v in proxy.witness[mark:] if isinstance(v['request'],dict) and v['request'].get('id')=='fresh-test']
        assert len(matches)==1,matches
        compare(raw,matches[0]['response'],result);trace[-1]['forwarding_witness']=matches[0]
    def options(url,path,window=128):return {'mode':'hybrid','upstream':url,'snapshot':str(path),'follow':'true','native_canonical_reads':'true','poll_ms':100,'window_blocks':window}

    with tempfile.TemporaryDirectory(prefix='quai-fresh-') as directory:
        directory=Path(directory)
        with scenario() as (url,profile,_),Proxy(url) as proxy:
            path=directory/'typed.json'
            with Server(extra=options(proxy.url,path)) as server:
                head=call(url,'quai_getBlockByNumber',['latest',True]);await_state(server,followed(head['hash']))
                receipts=[call(url,'quai_getTransactionReceipt',[v['hash']]) for v in head['transactions']]
                topic=next(v['logs'][0]['topics'][0] for v in receipts if v['logs'])
                address=next(v['logs'][0]['address'] for v in receipts if v['logs'])
                for receipt in receipts:parity(url,server.url,'quai_getTransactionReceipt',[receipt['transactionHash']])
                health=get_json(server.url+'/health');assert health['canonical_reads']['native_receipts']==3
                check('Quai Qi and ETX receipts use the native canonical path')
                filters=[{}, {'blockHash':head['hash']},{'fromBlock':'0x2','toBlock':'0x2'},
                    {'blockHash':head['hash'],'address':address},{'blockHash':head['hash'],'address':[address,address]},
                    {'blockHash':head['hash'],'topics':[topic]},{'blockHash':head['hash'],'topics':[[topic,'0x'+'ee'*32]]},
                    {'blockHash':head['hash'],'topics':[None]},{'blockHash':head['hash'],'topics':[[]]},
                    {'blockHash':head['hash'],'topics':[[None,'ignored invalid topic']]},
                    {'blockHash':head['hash'],'topics':['0x'+'ee'*32]}, {'blockHash':head['hash'],'address':'0x'+'ee'*20},
                    {'blockHash':head['hash'],'topics':[None,None]}]
                before=get_json(server.url+'/health')['canonical_reads']['native_logs']
                for value in filters:parity(url,server.url,'quai_getLogs',[value])
                assert get_json(server.url+'/health')['canonical_reads']['native_logs']==before+len(filters)
                check('native address topic wildcard empty and duplicate-address log filters match Go exactly')
                witnessed(proxy,server.url,'quai_getLogs',[{'fromBlock':'0x1','toBlock':'0x2'}])
                assert get_json(server.url+'/health')['canonical_reads']['fallbacks']['order']>=1
                check('multi-block log ordering falls back to Go with an exact forwarding witness')
                for method,params in [('quai_getTransactionReceipt',['0x'+'ee'*32]),('quai_getLogs',[{'blockHash':'0x'+'ee'*32}]),('quai_getLogs',[{'blockHash':head['hash'],'fromBlock':'0x0'}]),('quai_getLogs',[{'topics':[[[topic]]]}]),('quai_getLogs',[{'address':'bad'}]),('quai_getLogs',[{'fromBlock':'0x3','toBlock':'0x1'}]),('quai_getLogs',[{'fromBlock':'pending'}])]:
                    witnessed(proxy,server.url,method,params)
                check('misses malformed filters and unsupported tags preserve Go results and errors')
                proxy.mode='reverse';parity(url,server.url,'quai_getTransactionReceipt',[receipts[0]['transactionHash']]);proxy.mode=None
                check('freshness batch responses correlate by ID regardless of response order')
                for fault in ['proof-offline','malformed']:
                    proxy.mode=fault;witnessed(proxy,server.url,'quai_getTransactionReceipt',[receipts[0]['transactionHash']]);proxy.mode=None
                check('unavailable or malformed freshness proofs fall back without claiming a native hit')
                proxy.mode='identity'
                _,bad=request(server.url,'quai_getTransactionReceipt',[receipts[0]['transactionHash']])
                assert decode(bad['body'])['error']['code']==-32098
                proxy.mode=None
                assert get_json(server.url+'/health')['canonical_reads']['identity_rejections']==1
                check('network identity changes fail closed before a canonical result is returned')
                sdk_profile={**profile,'head_hash':head['hash'],'receipt_hashes':[v['transactionHash'] for v in receipts],'topic':topic}
                save(out/'sdk-profile.json',sdk_profile)
                for alias in ['quais57','quais58']:
                    reports=[]
                    for name,endpoint in [('go',url),('rust',server.url)]:
                        target=out/f'{alias}-{name}.json'
                        subprocess.run(['node',str(ROOT/'sdk/fresh_acceptance.cjs'),alias,endpoint,str(out/'sdk-profile.json'),str(target)],cwd=ROOT/'sdk',check=True,capture_output=True,text=True,timeout=60)
                        reports.append(json.loads(target.read_text()))
                    assert reports[0]==reports[1]
                    sdk.append({'alias':alias,'cases':len(reports[0]['results'])})
                check('both pinned SDKs decode native receipts and high-level getLogs results')
                # Hold ingestion so the per-request barrier must detect the stale view.
                proxy.gate.clear();select_scenario(url,'alternate')
                witnessed(proxy,server.url,'quai_getTransactionReceipt',[receipts[0]['transactionHash']])
                assert get_json(server.url+'/health')['canonical_reads']['fallbacks']['head_mismatch']>=1
                proxy.gate.set()
                alternate=call(url,'quai_getBlockByNumber',['latest',True]);await_state(server,followed(alternate['hash']))
                parity(url,server.url,'quai_getLogs',[{'blockHash':alternate['hash']}])
                check('same-height reorg closes the stale path until the replacement view is indexed')
                select_scenario(url,'canonical');await_state(server,followed(head['hash']))
                proxy.gate.clear();proxy.switch_at=proxy.proofs+2
                witnessed(proxy,server.url,'quai_getTransactionReceipt',[receipts[0]['transactionHash']])
                proxy.switch_at=None;proxy.gate.set();await_state(server,followed(alternate['hash']))
                check('a reorg between the two barriers discards the prepared receipt')
                before=get_json(server.url+'/health')['canonical_reads']['native_logs']
                proxy.mode='offline'
                _,bad=request(server.url,'quai_getLogs',[{'blockHash':alternate['hash']}])
                assert decode(bad['body'])['error']['code']==-32098
                assert get_json(server.url+'/health')['canonical_reads']['native_logs']==before
                assert call(server.url,'quai_getBlockByHash',[alternate['hash'],True])==alternate
                proxy.mode=None;await_state(server,followed(alternate['hash']))
                check('source outage never turns an old log view into a live result')
                save(out/'health.json',get_json(server.url+'/health'))
            # A bounded partial window cannot invent results for older history.
            with Server(extra=options(proxy.url,directory/'partial.json',window=1)) as partial:
                await_state(partial,followed(alternate['hash']))
                witnessed(proxy,partial.url,'quai_getLogs',[{'fromBlock':'0x0','toBlock':'0x1'}])
                parent=call(url,'quai_getBlockByNumber',['0x1',True])
                witnessed(proxy,partial.url,'quai_getTransactionReceipt',[parent['transactions'][0]['hash']])
                assert get_json(partial.url+'/health')['canonical_reads']['fallbacks']['coverage']>=2
                check('partial-window receipt and log misses use Go instead of false empty answers')
            save(out/'proxy-witnesses.json',proxy.witness)
        # A mined receipt and empty mined logs exercise the actual node backend.
        with FullNode(consensus='blake3',funded=True) as node:
            mining=mine(node,blocks=2)+mine(node,blocks=2,send=True)
            receipt=next(v['mined_transfer_receipt'] for v in mining if 'mined_transfer_receipt' in v)
            head=call(node.url,'quai_getBlockByNumber',['latest',True])
            path=directory/'mined.json'
            with Server(extra=options(node.url,path)) as server:
                await_state(server,followed(head['hash']))
                parity(node.url,server.url,'quai_getTransactionReceipt',[receipt['transactionHash']])
                parity(node.url,server.url,'quai_getLogs',[{'blockHash':receipt['blockHash']}])
                health=get_json(server.url+'/health');assert health['canonical_reads']['native_receipts']==1 and health['canonical_reads']['native_logs']==1
                server.process.kill();server.process.wait(timeout=5)
            with Server(extra=options(node.url,path)) as server:
                await_state(server,followed(head['hash']))
                parity(node.url,server.url,'quai_getTransactionReceipt',[receipt['transactionHash']])
            save(out/'mined-evidence.json',{'profile':node.profile,'mining':mining})
            check('mined Quai receipt and empty logs use the native path and survive restart')
    report={'passed':True,'run':run,'checks':checks,'strict_comparisons':len(trace),'sdk_comparisons':sum(v['cases'] for v in sdk),'sdk_groups':sdk,'source_hashes':{str(p.relative_to(ROOT)):sha(p) for directory in ['crates','scripts','reference','sdk'] for p in (ROOT/directory).rglob('*') if p.is_file() and 'node_modules' not in p.parts and p.suffix in ['.rs','.py','.go','.cjs']},'build_sha256':sha(ROOT/'results/build.json'),'sdk_lock_sha256':sha(ROOT/'sdk/package-lock.json'),'scope':'Strict single-block native logs and positive canonical receipts. Typed Qi/ETX cases are seeded; mined local Quai uses explicit test allocation/activation. Multi-block order is forwarded, no production or consensus claim.'}
    save(out/'trace.json',trace);save(out/'summary.json',report);save(ROOT/'results/latest-fresh.json',{'directory':str(out.relative_to(ROOT)),'passed':True})
    print(json.dumps({'passed':True,'checks':len(checks),'strict_comparisons':len(trace),'sdk_comparisons':report['sdk_comparisons'],'directory':str(out)},indent=2))


if __name__=='__main__':main()
