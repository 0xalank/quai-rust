"""Local process and differential-check helpers; no public services are used."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import random
import selectors
import signal
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

ROOT=Path(__file__).resolve().parents[1]
RUST=ROOT/'bin/quai-rpc'
GO=ROOT/'bin/quai-reference'
BASELINE={'workers':2,'batch_concurrency':1,'encoding':'value'}

class Server:
    def __init__(self,kind='rust',config=None,extra=None):
        self.kind,self.config,self.extra=kind,config or BASELINE,extra or {}
        self.process=None
        self.peak_rss_kib=0
        self.stop=threading.Event()
    def __enter__(self):
        args=[str(GO),'serve','--listen','127.0.0.1:0'] if self.kind=='go' else [str(RUST),'--listen','127.0.0.1:0']
        if self.kind=='rust':
            for key,value in dict(self.config,**self.extra).items():args += ['--'+key.replace('_','-'),str(value)]
        self.log=tempfile.TemporaryFile(mode='w+')
        self.process=subprocess.Popen(args,stdout=subprocess.PIPE,stderr=self.log,text=True,env=dict(os.environ,GOMAXPROCS='8'))
        sel=selectors.DefaultSelector();sel.register(self.process.stdout,selectors.EVENT_READ)
        try:
            if not sel.select(timeout=30):raise RuntimeError('server startup timed out')
            line=self.process.stdout.readline()
            try: ready=json.loads(line)
            except json.JSONDecodeError:
                self.log.seek(0);raise RuntimeError('server startup failed: '+line+self.log.read())
            if not ready.get('ready'):raise RuntimeError('server not ready')
            self.url='http://'+ready['address']
            self.monitor=threading.Thread(target=self._monitor,daemon=True);self.monitor.start()
            return self
        except BaseException:
            self.__exit__(None,None,None);raise
        finally:sel.close()
    def _monitor(self):
        while not self.stop.wait(.1):
            try:
                p=subprocess.run(['ps','-o','rss=','-p',str(self.process.pid)],capture_output=True,text=True,timeout=2)
                if p.returncode==0 and p.stdout.strip():self.peak_rss_kib=max(self.peak_rss_kib,int(p.stdout.strip()))
            except (ValueError,subprocess.TimeoutExpired):pass
    def __exit__(self,*_):
        self.stop.set()
        if hasattr(self,'monitor'):self.monitor.join(timeout=3)
        if self.process and self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
            try:self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:self.process.kill();self.process.wait(timeout=5)
        if self.process and self.process.stdout:self.process.stdout.close()
        if hasattr(self,'log'):self.log.close()

def rpc(url,body,content_type='application/json'):
    data=body.encode() if isinstance(body,str) else json.dumps(body,separators=(',',':'),ensure_ascii=False).encode()
    req=urllib.request.Request(url,data=data,headers={'Content-Type':content_type})
    try:
        with urllib.request.urlopen(req,timeout=15) as response:status,raw=response.status,response.read()
    except urllib.error.HTTPError as error:status,raw=error.code,error.read()
    if not raw:return status,None
    try:return status,json.loads(raw)
    except json.JSONDecodeError:return status,raw.decode(errors='replace')

def request(method,params=None,id=1):
    value={'jsonrpc':'2.0','method':method,'id':id}
    if params is not None:value['params']=params
    return value

def normalize(value):
    if isinstance(value,list):return sorted((normalize(v) for v in value),key=lambda v:json.dumps(v,sort_keys=True))
    if isinstance(value,dict):
        result=dict(value)
        # Error wording differs between implementations; code/data and IDs must match.
        if isinstance(result.get('error'),dict):result['error']={k:v for k,v in result['error'].items() if k!='message'}
        return result
    return value

def cases(seed=20261103,generated=32):
    vectors=[request('bench_blockNumber'),request('bench_payload',[42]),request('bench_echo',[0]),
        request('bench_echo',[18446744073709551615],9007199254740993123456789),
        request('bench_echo',[37],'quoted"\n雪'),request('bench_null',id=None),
        request('bench_failure'),request('absent'),request('bench_echo',[]),
        request('bench_echo',[-1]),request('bench_payload',['wrong']),request('bench_payload',[1,2]),
        request('bench_blockNumber',[1]),[],{},'{',
        {'jsonrpc':'2.0','method':'bench_echo','params':[7]},
        [{'jsonrpc':'2.0','method':'bench_null'}],
        [request('bench_payload',[9],'a'),request('bench_failure',id='b'),
         {'jsonrpc':'2.0','method':'bench_echo','params':[8]},request('bench_null',id=4)],
        request('quai_chainId'),request('quai_blockNumber'),request('quai_getBalance',['0x'+'01'*20,'latest'])]
    rng=random.Random(seed)
    for i in range(generated):
        batch=[]
        for j in range(rng.randint(1,8)):
            method=rng.choice(['bench_echo','bench_payload','bench_null','bench_failure','bench_blockNumber'])
            params=[rng.randrange(2**64)] if method in ['bench_echo','bench_payload'] else []
            item=request(method,params,id=rng.choice([i*100+j,f'id-{i}-{j}',None]))
            if rng.random()<.15:del item['id']
            batch.append(item)
        vectors.append(batch if rng.random()<.7 else batch[0])
    return vectors

def check_fixture(url,oracle,seed=20261103,generated=32):
    count=0
    for case in cases(seed,generated):
        a_status,a=rpc(oracle,case);b_status,b=rpc(url,case)
        assert 200<=a_status<300 and 200<=b_status<300,(case,a_status,b_status)
        assert normalize(a)==normalize(b),{'request':case,'go':a,'rust':b}
        count+=1
    return count

def bench(url,profile):
    args=[str(GO),'bench','--url',url]
    for key,value in profile.items():
        if key!='name':args+=['--'+key.replace('_','-'),str(value)]
    p=subprocess.run(args,capture_output=True,text=True,timeout=130,env=dict(os.environ,GOMAXPROCS='8'))
    if p.returncode:raise RuntimeError(p.stderr+p.stdout)
    result=json.loads(p.stdout)
    assert result['failed_requests']==0
    return dict(result,name=profile['name'])

def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def save(path,value):
    path=Path(path);path.parent.mkdir(parents=True,exist_ok=True)
    path.write_text(json.dumps(value,indent=2)+'\n')

def verify_contract():
    path=ROOT/'fixtures/contract-sha256.json'
    hashes=json.loads(path.read_text())
    for name,digest in hashes.items():
        if sha(ROOT/name)!=digest:raise RuntimeError(f'Benchmark contract changed: {name}; review before resealing')
    return hashes
