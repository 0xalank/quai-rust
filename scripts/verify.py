#!/usr/bin/env python3
import contextlib
import datetime
import http.server
import json
import threading
from lab import BASELINE,ROOT,Server,check_fixture,request,rpc,save

def main():
    checks={}
    with Server('go') as oracle:
        for name,config in [('baseline',BASELINE),('raw-parallel',dict(BASELINE,encoding='raw',batch_concurrency=8)),
                            ('four-workers',dict(BASELINE,workers=4,encoding='raw',batch_concurrency=4))]:
            with Server(config=config) as rust:
                checks[name]=check_fixture(rust.url,oracle.url,generated=64)
        with Server(extra={'mode':'gateway','upstream':oracle.url,'batch_concurrency':8}) as gateway:
            for method,params in [('quai_chainId',[]),('quai_blockNumber',[]),('quai_getBalance',['0x'+'01'*20,'latest'])]:
                _,expected=rpc(oracle.url,request(method,params,'gateway'))
                _,actual=rpc(gateway.url,request(method,params,'gateway'))
                assert actual==expected,(actual,expected)
            for method in ['quai_sendRawTransaction','admin_nodeInfo','debug_traceBlock','personal_unlockAccount']:
                _,value=rpc(gateway.url,request(method));assert value['error']['code']==-32601
            _,value=rpc(gateway.url,[request('quai_chainId',id='a'),request('quai_blockNumber',id='b')])
            assert len(value)==2 and [v['id'] for v in value]==['a','b']
            checks['gateway_against_go']=8
    with Server() as rust:
        for raw in ['null','3',request('bench_null',id=True),{'jsonrpc':'1.0','method':'bench_null','id':1}]:
            _,value=rpc(rust.url,raw);assert value['error']['code']==-32600
        status,_=rpc(rust.url,request('bench_null'),content_type='text/plain');assert status==415
        _,value=rpc(rust.url,[request('bench_null',id=i) for i in range(129)]);assert value['error']['code']==-32600
        status,_=rpc(rust.url,'x'*(1024*1024+1));assert status==413
        checks['strict_protocol_and_limits']=7
    with Server(extra={'timeout_ms':5,'max_inflight':1,'batch_concurrency':8}) as rust:
        _,value=rpc(rust.url,[request('bench_delay',[100],1),request('bench_delay',[100],2)])
        assert {v['error']['code'] for v in value}=={-32002,-32005}
        checks['timeout_and_backpressure']=2
    with Server(extra={'max_response_bytes':128}) as rust:
        _,value=rpc(rust.url,request('bench_payload',[0]));assert value['error']['code']==-32005
        checks['response_budget']=1
    class BadUpstream(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if data['method']=='quai_getCode':body=json.dumps({'jsonrpc':'2.0','id':'wrong','result':'0x'}).encode()
            elif data['method']=='quai_getStorageAt':body=b'not json'
            elif data['method']=='quai_getLogs':body=b'x'*4096
            else:body=json.dumps({'jsonrpc':'2.0','id':data['id'],'error':{'code':-32077,'message':'source error','data':{'height':7}}}).encode()
            self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
        def log_message(self,*_):pass
    upstream=http.server.ThreadingHTTPServer(('127.0.0.1',0),BadUpstream)
    thread=threading.Thread(target=upstream.serve_forever,daemon=True);thread.start()
    try:
        with Server(extra={'mode':'gateway','upstream':f'http://127.0.0.1:{upstream.server_port}','max_response_bytes':1024}) as rust:
            for method,code in [('quai_getCode',-32098),('quai_getStorageAt',-32098),('quai_getLogs',-32005)]:
                _,value=rpc(rust.url,request(method));assert value['error']['code']==code
            _,value=rpc(rust.url,request('quai_chainId',id='upstream-error'))
            assert value['error']=={'code':-32077,'message':'source error','data':{'height':7}}
            assert value['id']=='upstream-error'
            checks['upstream_envelope_limits_and_errors']=4
    finally:upstream.shutdown();upstream.server_close();thread.join(timeout=3)
    result={'verified_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'passed':True,
            'checks':checks,'scope':'Synthetic fixtures, Go differential results, strict JSON-RPC edges and local read-only gateway. No real chain/consensus parity.'}
    save(ROOT/'results/conformance.json',result)
    print(json.dumps(result,indent=2))

if __name__=='__main__':main()
