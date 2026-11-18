#!/usr/bin/env python3
"""Standalone RPC/protocol acceptance with no Go process, checkout or upstream."""
import argparse
import contextlib
import datetime
import json
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from lab import ROOT,save,sha

@contextlib.contextmanager
def server(snapshot):
    with tempfile.TemporaryFile(mode='w+') as errors:
        process=subprocess.Popen([str(ROOT/'bin/quai-rpc'),'--listen','127.0.0.1:0','--snapshot',str(snapshot)],stdout=subprocess.PIPE,stderr=errors,text=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout,selectors.EVENT_READ)
                if not selector.select(10):raise RuntimeError('Rust startup timeout')
            line=process.stdout.readline()
            if not line:errors.seek(0);raise RuntimeError(errors.read())
            ready=json.loads(line);yield 'http://'+ready['address'],process
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
            process.stdout.close()

def exchange(url,request=None):
    if request is None:
        with urllib.request.urlopen(url,timeout=5) as response:return json.load(response)
    raw=json.dumps(request).encode()
    with urllib.request.urlopen(urllib.request.Request(url,data=raw,headers={'Content-Type':'application/json'}),timeout=5) as response:return json.load(response)

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--sdk',action='store_true');args=parser.parse_args()
    build=json.loads((ROOT/'results/rust-build.json').read_text())
    assert build['profile']=='standalone-rust' and not build['go_runtime_dependency']
    for name,digest in build['binaries'].items():assert sha(ROOT/'bin'/name)==digest
    for name,digest in build['rust_sources'].items():assert sha(ROOT/name)==digest
    protocol=json.loads(subprocess.check_output([str(ROOT/'bin/quai-protocol-check'),str(ROOT/'fixtures/protocol/work-headers.json')],text=True));assert protocol['passed']
    transaction_protocol=json.loads(subprocess.check_output([str(ROOT/'bin/quai-protocol-check'),str(ROOT/'fixtures/protocol/transactions.json')],text=True));assert transaction_protocol['passed']
    foundation_protocol=json.loads(subprocess.check_output([str(ROOT/'bin/quai-protocol-check'),str(ROOT/'fixtures/protocol/foundation.json')],text=True));assert foundation_protocol['passed']
    run=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ');out=ROOT/'results'/('rust-'+run);out.mkdir(parents=True)
    trace=[];checks=[];sdk=[]
    def check(name):checks.append(name);print(name,flush=True)
    def rpc(url,method,params,expected):
        request={'jsonrpc':'2.0','id':'standalone','method':method,'params':params};result=exchange(url,request)
        assert result=={'jsonrpc':'2.0','id':'standalone','result':expected},(method,result)
        trace.append({'request':request,'response':result})
    def payload(path):return json.loads(path.read_text())['payload']
    def view_checks(url,view):
        blocks={v['hash']:v for v in view['blocks']};canonical=set(view['canonical']);receipt_by_tx={v['transactionHash']:v for v in view['receipts'] if v['blockHash'] in canonical}
        for block in view['blocks']:
            hashed=dict(block)
            for key in ['transactions','outboundEtxs']:hashed[key]=[v['hash'] for v in block[key]]
            for full,want in [(True,block),(False,hashed)]:rpc(url,'quai_getBlockByHash',[block['hash'],full],want)
            if block['hash'] in canonical:rpc(url,'quai_getBlockByNumber',[block['woHeader']['number'],True],block)
        for receipt in view['receipts']:rpc(url,'quai_getTransactionReceipt',[receipt['transactionHash']],receipt_by_tx.get(receipt['transactionHash']))
        for key in view['canonical']:
            block=blocks[key];logs=[]
            for tx in block['transactions']:logs+=receipt_by_tx[tx['hash']]['logs']
            rpc(url,'quai_getLogs',[{'blockHash':key}],logs)
        rpc(url,'quai_getTransactionReceipt',['0x'+'ee'*32],None)
        rpc(url,'quai_chainId',[],view['identity']['chain_id'])
    with tempfile.TemporaryDirectory(prefix='quai-rust-only-') as directory:
        path=Path(directory)/'view.json';path.write_bytes((ROOT/'fixtures/native/scenario-a.json').read_bytes())
        with server(path) as (url,process):
            view_checks(url,payload(path));check('Rust-only block receipt and single-block log reads match frozen reference values')
            health=exchange(url+'/health');assert health['runtime_profile']=='standalone-rust' and not health['go_runtime_dependency'] and not health['chain_verified'] and health['upstream_calls']==0
            assert health['canonical_reads']['freshness_http_calls']==0;check('runtime reports frozen unverified view and performs zero Go freshness/forwarding calls')
            for method,params in [('quai_getBalance',['0x'+'00'*20,'latest']),('quai_getTransactionCount',['0x'+'00'*20,'latest'])]:
                result=exchange(url,{'jsonrpc':'2.0','id':1,'method':method,'params':params});assert result['error']['code']==-32004
            result=exchange(url,{'jsonrpc':'2.0','id':1,'method':'quai_sendRawTransaction','params':['0x']});assert result['error']['code']==-32601
            check('unimplemented state calls report unavailable and signing/submission stays denied')
            if args.sdk:
                source=json.loads((ROOT/'fixtures/protocol/sdk-source.json').read_text());assert source['sdk_lock_sha256']==sha(ROOT/'sdk/package-lock.json')
                for alias in ['quais57','quais58']:
                    target=out/(alias+'.json')
                    result=subprocess.run(['node',str(ROOT/'sdk/fresh_acceptance.cjs'),alias,url,str(ROOT/'fixtures/protocol/sdk-profile.json'),str(target)],cwd=ROOT/'sdk',text=True,capture_output=True,timeout=60)
                    if result.returncode:raise RuntimeError(result.stderr+result.stdout)
                    actual=json.loads(target.read_text());expected=json.loads((ROOT/f'fixtures/protocol/{alias}-expected.json').read_text());assert actual==expected
                    sdk.append({'alias':alias,'cases':len(actual['results'])})
                check('both pinned SDKs match previously recorded direct-Go receipt/log outputs without a Go server')
            path.write_bytes((ROOT/'fixtures/native/scenario-b.json').read_bytes());process.send_signal(signal.SIGHUP)
            deadline=time.monotonic()+5
            while exchange(url+'/health')['store']['generation']!=2:
                assert time.monotonic()<deadline;time.sleep(.02)
            view_checks(url,payload(path));check('Rust atomically swaps canonical branch and receipt/log indexes without Go')
            path.write_bytes(b'{"truncated":');process.send_signal(signal.SIGHUP)
            deadline=time.monotonic()+5
            while exchange(url+'/health')['reload_failures']==0:
                assert time.monotonic()<deadline;time.sleep(.02)
            assert exchange(url+'/health')['store']['generation']==2;check('failed Rust import preserves the previous valid view')
        path.write_bytes((ROOT/'fixtures/native/scenario-b.json').read_bytes())
        with server(path) as (url,_):view_checks(url,payload(path))
        check('standalone service restarts from its persisted frozen view')
    for arguments in [['--mode','gateway','--upstream','http://127.0.0.1:1'],['--mode','hybrid','--snapshot','missing.json','--upstream','http://127.0.0.1:1'],['--follow','true','--snapshot','missing.json'],['--native-canonical-reads','true','--snapshot','missing.json']]:
        rejected=subprocess.run([str(ROOT/'bin/quai-rpc'),*arguments],text=True,capture_output=True,timeout=5)
        assert rejected.returncode==2 and 'excluded' in rejected.stderr
    check('shipping binary rejects every Go-backed mode before opening an upstream')
    report={'passed':True,'profile':'standalone-rust','go_runtime_calls':0,'checks':checks,'rpc_comparisons':len(trace),'sdk_comparisons':sum(v['cases'] for v in sdk),'sdk_groups':sdk,'protocol':protocol,'transaction_protocol':transaction_protocol,'foundation_protocol':foundation_protocol,'build_sha256':sha(ROOT/'results/rust-build.json'),'source_hashes':{str(p.relative_to(ROOT)):sha(p) for d in ['crates','scripts'] for p in (ROOT/d).rglob('*') if p.is_file() and p.suffix in ['.rs','.py']},'scope':'No-Go build/runtime profile; frozen imported RPC views plus bounded native protocol codec/hash vectors. Not independent sync or full block validation.'}
    save(out/'trace.json',trace);save(out/'summary.json',report);save(ROOT/'results/latest-rust.json',{'directory':str(out.relative_to(ROOT)),'passed':True})
    print(json.dumps({'passed':True,'checks':len(checks),'rpc_comparisons':len(trace),'sdk_comparisons':report['sdk_comparisons'],'protocol_checks':protocol['checks']+transaction_protocol['checks']+foundation_protocol['checks'],'directory':str(out)},indent=2))

if __name__=='__main__':main()
