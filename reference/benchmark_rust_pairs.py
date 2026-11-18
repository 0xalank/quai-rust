#!/usr/bin/env python3
"""Fixed offline paired Rust comparison; validates every immutable read result."""
import argparse,contextlib,datetime,json,math,os
from pathlib import Path
import selectors,signal,statistics,subprocess,sys,tempfile
p=argparse.ArgumentParser();p.add_argument('--trial',required=True);p.add_argument('--project',default=str(Path(__file__).resolve().parents[1]));a=p.parse_args()
root=Path(a.project).resolve();trial=Path(a.trial).resolve();candidate=trial/'candidate'
sys.path.insert(0,str(root/'scripts'))
from benchmark_native import Sampler
from lab import save,sha
from fullnode import call
from compat_rpc import get_json
contract=json.loads((trial/'contract.json').read_text());fixed=root/contract['workload_source'];original=json.loads((fixed/'summary.json').read_text())
client=Path(contract['client']);assert sha(client)==original['contract']['load_generator']
builds={};acceptance={};binaries={'baseline':trial/'baseline/quai-rpc','candidate':candidate/'bin/quai-rpc'}
for mode in binaries:
 builds[mode]=json.loads((trial/'baseline/build.json' if mode=='baseline' else candidate/'results/rust-build.json').read_text())
 assert sha(binaries[mode])==builds[mode]['binaries']['quai-rpc']
 if mode=='baseline':acceptance[mode]=json.loads((trial/'baseline/acceptance.json').read_text())
 else:
  last=json.loads((candidate/'results/latest-rust.json').read_text());assert last['passed'];acceptance[mode]=json.loads((candidate/last['directory']/'summary.json').read_text())
 assert acceptance[mode]['passed'] and acceptance[mode]['sdk_comparisons']==16 and acceptance[mode]['protocol']['checks']==1711
 assert not builds[mode]['go_runtime_dependency']
assert sha(binaries['baseline'])==contract['baseline_binary_sha256']
for name,digest in builds['candidate']['rust_sources'].items():assert sha(candidate/name)==digest
snapshot=root/'fixtures/native/scenario-a.json';assert sha(snapshot)==original['contract']['snapshot']
workloads=[('block','quai_getBlockByHash'),('receipt-0x0','quai_getTransactionReceipt'),('receipt-0x1','quai_getTransactionReceipt'),('receipt-0x2','quai_getTransactionReceipt'),('logs','quai_getLogs')]
files=[fixed/w/f for w,_ in workloads for f in ['params.json','expected.json']]
pinned={str(v):sha(v) for v in files};pinned[str(client)]=sha(client);pinned[str(snapshot)]=sha(snapshot);pinned[str(Path(__file__).resolve())]=sha(Path(__file__))
for name in ['benchmark_native.py','lab.py','fullnode.py','compat_rpc.py']:pinned[str(root/'scripts'/name)]=sha(root/'scripts'/name)
save(trial/'measurement-contract.json',{'experiment':contract,'pinned_sha256':pinned,'builds':builds})
@contextlib.contextmanager
def service(binary):
 with tempfile.TemporaryFile(mode='w+') as log:
  proc=subprocess.Popen([str(binary),'--listen','127.0.0.1:0','--snapshot',str(snapshot)],stdout=subprocess.PIPE,stderr=log,text=True,cwd=root)
  try:
   with selectors.DefaultSelector() as sel:
    sel.register(proc.stdout,selectors.EVENT_READ)
    if not sel.select(15):raise RuntimeError('server readiness timeout')
   line=proc.stdout.readline()
   if not line:log.seek(0);raise RuntimeError(log.read())
   ready=json.loads(line);assert ready['ready'];yield 'http://'+ready['address'],proc
  finally:
   if proc.poll() is None:
    proc.send_signal(signal.SIGINT)
    try:proc.wait(timeout=5)
    except subprocess.TimeoutExpired:proc.kill();proc.wait(timeout=5)
   proc.stdout.close()
records=[]
with contextlib.ExitStack() as stack:
 servers={mode:stack.enter_context(service(binary)) for mode,binary in binaries.items()}
 for name,method in workloads:
  directory=trial/name;directory.mkdir();params=json.loads((fixed/name/'params.json').read_text());expected=json.loads((fixed/name/'expected.json').read_text())
  for mode,(url,proc) in servers.items():assert call(url,method,params)==expected,(name,mode)
  for number in range(1,contract['rounds']+1):
   order=['baseline','candidate'] if number%2 else ['candidate','baseline']
   for mode in order:
    print(f'{name} round {number}/{contract["rounds"]}: {mode}',flush=True)
    url,proc=servers[mode];before=get_json(url+'/health')
    with Sampler([proc.pid]) as resources:
     result=subprocess.run([str(client),'--url',url,'--method',method,'--params',str(fixed/name/'params.json'),'--expected',str(fixed/name/'expected.json'),'--seconds',str(contract['seconds']),'--concurrency',str(contract['concurrency'])],capture_output=True,text=True,check=True,timeout=35,env={**os.environ,'GOMAXPROCS':'8'})
     v=json.loads(result.stdout);assert v['failed_requests']==0 and v['validated_every_response']
    after=get_json(url+'/health');assert after['upstream_calls']==after['canonical_reads']['freshness_http_calls']==after['canonical_reads']['freshness_rpc_calls']==0
    assert not after['go_runtime_dependency'] and after['native_hits']-before['native_hits']>=v['calls']
    record={**v,'mode':mode,'workload':name,'round':number,'server_cpu_seconds':resources.cpu,'server_cpu_ms_per_call':1000*resources.cpu/v['calls'],'sampled_peak_server_rss_kib':resources.peak,'health_before':before,'health_after':after,'load_average':os.getloadavg()}
    records.append(record);save(directory/f'{number}-{mode}.json',record)
medians=[];comparisons=[]
for name,_ in workloads:
 by={}
 for mode in binaries:
  group=[v for v in records if v['workload']==name and v['mode']==mode]
  by[mode]={key:statistics.median(v[key] for v in group) for key in ['calls_per_second','p99_ms','server_cpu_ms_per_call']};by[mode]['peak_rss_kib']=max(v['sampled_peak_server_rss_kib'] for v in group)
  medians.append({'workload':name,'mode':mode,**by[mode]})
 base,cand=by['baseline'],by['candidate'];ratio=cand['calls_per_second']/base['calls_per_second'];latency=cand['p99_ms']<=base['p99_ms']*1.05+1
 comparisons.append({'workload':name,'qps_ratio':ratio,'p99_guard_passed':latency,'qps_guard_passed':ratio>=1.03,'server_cpu_ratio':cand['server_cpu_ms_per_call']/base['server_cpu_ms_per_call']})
for name,digest in pinned.items():assert sha(Path(name))==digest,name
for mode in binaries:assert sha(binaries[mode])==builds[mode]['binaries']['quai-rpc']
report={'run':contract['run'],'validated_calls':sum(v['calls'] for v in records),'failed_requests':sum(v['failed_requests'] for v in records),'medians':medians,'comparisons':comparisons,'geomean_qps_ratio':math.exp(statistics.mean(math.log(v['qps_ratio']) for v in comparisons)),'promote':all(v['qps_guard_passed'] and v['p99_guard_passed'] for v in comparisons),'records':records,'contract':contract,'builds':builds,'scope':'Paired standalone Rust warm frozen reads on a shared host. Identical offline Go load client and result validation; no Go service or upstream. Three two-second samples per workload/implementation; concurrency 8. Server CPU sampling includes warmup, client CPU is separate. Not EVM or full-node throughput.'}
save(trial/'summary.json',report)
print(json.dumps({k:v for k,v in report.items() if k not in ['records','builds','contract']},indent=2))
