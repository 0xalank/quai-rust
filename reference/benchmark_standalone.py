#!/usr/bin/env python3
"""Offline comparison of the shipping Rust read service and pinned Go APIs."""
import argparse
import contextlib
import datetime
import json
import os
from pathlib import Path
import selectors
import signal
import statistics
import subprocess
import sys
import time

p=argparse.ArgumentParser()
p.add_argument('--project',default=str(Path(__file__).resolve().parents[1]))
p.add_argument('--reference-lab',required=True)
a=p.parse_args()
root=Path(a.project).resolve();reference=Path(a.reference_lab).resolve()
sys.path.insert(0,str(root/'scripts'))
from benchmark_native import Sampler
from lab import save,sha
from fullnode import call
from compat_rpc import get_json

@contextlib.contextmanager
def service(command,cwd):
    with __import__('tempfile').TemporaryFile(mode='w+') as log:
        process=subprocess.Popen(command,cwd=cwd,stdout=subprocess.PIPE,stderr=log,text=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout,selectors.EVENT_READ)
                if not selector.select(15):raise RuntimeError('benchmark server readiness timeout')
            line=process.stdout.readline()
            if not line:log.seek(0);raise RuntimeError(log.read())
            ready=json.loads(line);assert ready['ready']
            yield 'http://'+ready['address'],process
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
            process.stdout.close()

build=json.loads((root/'results/rust-build.json').read_text())
assert not build['go_runtime_dependency']
for name,digest in build['binaries'].items():assert sha(root/'bin'/name)==digest,name
for name,digest in build['rust_sources'].items():assert sha(root/name)==digest,name
accepted=json.loads((root/'results/latest-rust.json').read_text());assert accepted['passed']
acceptance=json.loads((root/accepted['directory']/'summary.json').read_text());assert acceptance['build_sha256']==sha(root/'results/rust-build.json')
reference_build=json.loads((reference/'results/build.json').read_text())
for name in ['quai-scenario-oracle','quai-fresh-load']:assert sha(reference/'bin'/name)==reference_build['binaries'][name],name
assert sha(root/'reference/freshload/main.go')==reference_build['reference_sources']['reference/freshload/main.go']
snapshot=root/'fixtures/native/scenario-a.json';view=json.loads(snapshot.read_text())['payload'];head=view['canonical'][-1]
contract={'benchmark_script':sha(Path(__file__)),'snapshot':sha(snapshot),'load_generator':sha(reference/'bin/quai-fresh-load'),'load_source':sha(root/'reference/freshload/main.go'),'go_api_reference':sha(reference/'bin/quai-scenario-oracle'),'rust_binary':sha(root/'bin/quai-rpc'),'cargo_lock':sha(root/'Cargo.lock')}
run=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')+'-standalone-speed';out=root/'results'/run;out.mkdir(parents=True,exist_ok=False)
workloads=[('block','quai_getBlockByHash',[head,True])]
workloads += [('receipt-'+v['type'],'quai_getTransactionReceipt',[v['transactionHash']]) for v in view['receipts'] if v['blockHash']==head]
workloads += [('logs','quai_getLogs',[{'blockHash':head}])]
records=[]
with service([str(reference/'bin/quai-scenario-oracle'),'--listen','127.0.0.1:0'],reference) as (go_url,go),service([str(root/'bin/quai-rpc'),'--listen','127.0.0.1:0','--snapshot',str(snapshot)],root) as (rust_url,rust):
    before=get_json(rust_url+'/health');assert before['runtime_profile']=='standalone-rust' and not before['go_runtime_dependency']
    assert call(go_url,'quai_getBlockByNumber',['latest',False])['hash']==head
    for name,method,params in workloads:
        expected=call(go_url,method,params);assert call(rust_url,method,params)==expected,name
        directory=out/name;directory.mkdir();save(directory/'params.json',params);save(directory/'expected.json',expected)
        for round_number in range(1,4):
            order=['go','rust'] if round_number%2 else ['rust','go']
            for mode in order:
                print(f'{name} round {round_number}/3: {mode}',flush=True)
                url,server=(go_url,go) if mode=='go' else (rust_url,rust)
                health_before=get_json(rust_url+'/health') if mode=='rust' else None
                with Sampler([server.pid]) as resources:
                    result=subprocess.run([str(reference/'bin/quai-fresh-load'),'--url',url,'--method',method,'--params',str(directory/'params.json'),'--expected',str(directory/'expected.json'),'--seconds','2','--concurrency','8'],capture_output=True,text=True,check=True,timeout=35,env={**os.environ,'GOMAXPROCS':'8'})
                    values=json.loads(result.stdout);assert values['failed_requests']==0
                health_after=get_json(rust_url+'/health') if mode=='rust' else None
                if mode=='rust':
                    assert health_after['upstream_calls']==0 and health_after['canonical_reads']['freshness_http_calls']==0
                    assert health_after['native_hits']-health_before['native_hits']>=values['calls']
                record={**values,'workload':name,'mode':mode,'round':round_number,'server_cpu_seconds':resources.cpu,'server_cpu_ms_per_call':resources.cpu*1000/values['calls'],'sampled_peak_server_rss_kib':resources.peak,'health_before':health_before,'health_after':health_after,'params':params,'expected_sha256':sha(directory/'expected.json'),'load_average':os.getloadavg()}
                records.append(record);save(directory/f'{round_number}-{mode}.json',record)
    final_health=get_json(rust_url+'/health');assert final_health['upstream_calls']==0 and final_health['canonical_reads']['freshness_http_calls']==0
medians=[]
for name,_,_ in workloads:
    for mode in ['go','rust']:
        values=[v for v in records if v['workload']==name and v['mode']==mode]
        medians.append({'workload':name,'mode':mode,**{key:statistics.median(v[key] for v in values) for key in ['calls_per_second','p99_ms','server_cpu_ms_per_call']},'peak_rss_kib':max(v['sampled_peak_server_rss_kib'] for v in values),'qps_min':min(v['calls_per_second'] for v in values),'qps_max':max(v['calls_per_second'] for v in values)})
assert sha(root/'bin/quai-rpc')==contract['rust_binary'] and sha(snapshot)==contract['snapshot'] and sha(Path(__file__))==contract['benchmark_script']
report={'run':run,'validated_calls':sum(v['calls'] for v in records),'failed_requests':sum(v['failed_requests'] for v in records),'medians':medians,'records':records,'rust_health':final_health,'contract':contract,'rust_build':build,'reference_build':reference_build,'acceptance_report':accepted['directory']+'/summary.json','scope':'Warm frozen read models on shared macOS, concurrency 8, three two-second repetitions per mode/workload. Same Go load generator validates every response. Go server is a controlled seeded API oracle; Rust has no Go upstream. Server CPU/RSS excludes the load generator and the other idle implementation. Not independent sync, consensus validation, ingestion, cold storage, block execution or production TPS. Percentiles are medians of per-run percentiles.'}
save(out/'summary.json',report);save(root/'results/latest-standalone-speed.json',{'directory':str(out.relative_to(root))})
print(json.dumps({k:v for k,v in report.items() if k in ['run','validated_calls','failed_requests','medians']},indent=2))
