#!/usr/bin/env python3
"""Separate baseline for strict freshness barriers; timed workloads are sequential."""
import contextlib
import datetime
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

from benchmark_native import QuietServer, Sampler
from compat_rpc import get_json
from export_snapshot import atomic_write, capture, seal
from fullnode import call
from lab import BASELINE, ROOT, save, sha
from next_lab import scenario


def main():
    names=['scripts/benchmark_fresh.py','reference/freshload/main.go','scripts/next_lab.py','reference/scenarios/main.go','scripts/benchmark_native.py']
    contract={name:sha(ROOT/name) for name in names}
    build=json.loads((ROOT/'results/build.json').read_text())
    for name,digest in build['binaries'].items():assert sha(ROOT/'bin'/name)==digest,name
    for name,digest in build['rust_sources'].items():assert sha(ROOT/name)==digest,name
    acceptance=json.loads((ROOT/'results/latest-fresh.json').read_text());assert acceptance['passed']
    accepted=json.loads((ROOT/acceptance['directory']/'summary.json').read_text());assert accepted['build_sha256']==sha(ROOT/'results/build.json')
    run=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')+'-fresh-benchmark'
    out=ROOT/'results'/run;out.mkdir(parents=True,exist_ok=False)
    records=[]
    with scenario() as (source,profile,process):
        view=capture(source);snapshot=out/'export.json';atomic_write(snapshot,seal(view))
        head=view['canonical'][-1]
        workloads=[('receipt-'+v['type'],'quai_getTransactionReceipt',[v['transactionHash']]) for v in view['receipts'] if v['blockHash']==head]
        workloads.append(('logs-single','quai_getLogs',[{'blockHash':head}]))
        for workload,method,params in workloads:
            expected=call(source,method,params);save(out/'params.json',params);save(out/'expected.json',expected)
            for round_number in range(3):
                modes=['go','follow-forward','follow-native'];modes=modes[round_number:]+modes[:round_number]
                for mode in modes:
                    print(f'{workload} round {round_number+1}/3: {mode}',flush=True)
                    with contextlib.ExitStack() as stack:
                        server=None
                        if mode=='go':url=source;pids=[process.pid]
                        else:
                            path=out/f'{workload}-{round_number}-{mode}.json';atomic_write(path,seal(view))
                            server=stack.enter_context(QuietServer(config=BASELINE,extra={'mode':'hybrid','upstream':source,'snapshot':str(path),'follow':'true','native_canonical_reads':str(mode=='follow-native').lower(),'poll_ms':1000}))
                            url=server.url;pids=[process.pid,server.process.pid]
                            deadline=time.monotonic()+5
                            while not get_json(url+'/health')['ingestion'].get('network_verified'):
                                assert time.monotonic()<deadline;time.sleep(.02)
                        before=get_json(url+'/health') if server else None
                        with Sampler(pids) as usage:
                            result=subprocess.run([str(ROOT/'bin/quai-fresh-load'),'--url',url,'--method',method,'--params',str(out/'params.json'),'--expected',str(out/'expected.json'),'--seconds','2','--concurrency','8'],capture_output=True,text=True,check=True,timeout=35,env={**os.environ,'GOMAXPROCS':'8'})
                            value=json.loads(result.stdout);assert value['failed_requests']==0
                        after=get_json(url+'/health') if server else None
                        if mode=='follow-native':
                            key='native_logs' if method=='quai_getLogs' else 'native_receipts'
                            assert after['canonical_reads'][key]-before['canonical_reads'][key]>=value['calls']
                            assert after['upstream_calls']==before['upstream_calls'],'unexpected forwarding during native timing'
                        record={**value,'workload':workload,'mode':mode,'round':round_number+1,'server_cpu_seconds':usage.cpu,'server_cpu_ms_per_call':usage.cpu*1000/value['calls'],'sampled_peak_server_rss_kib':usage.peak,'health_before':before,'health_after':after,'expected_sha256':sha(out/'expected.json'),'params':params,'load_average':os.getloadavg()}
                        records.append(record);save(out/f'{workload}-{round_number}-{mode}-timing.json',record)
    for name,digest in contract.items():assert sha(ROOT/name)==digest,name
    medians=[]
    for workload in sorted({v['workload'] for v in records}):
        for mode in ['go','follow-forward','follow-native']:
            group=[v for v in records if v['workload']==workload and v['mode']==mode]
            medians.append({'workload':workload,'mode':mode,**{k:statistics.median(v[k] for v in group) for k in ['calls_per_second','p99_ms','server_cpu_ms_per_call']}})
    report={'run':run,'validated_calls':sum(v['calls'] for v in records),'failed_requests':sum(v['failed_requests'] for v in records),'medians':medians,'records':records,'contract_source_hashes':contract,'build':build,'acceptance_report':acceptance['directory']+'/summary.json','scope':'Warm loopback seeded Go APIs on shared macOS; 3x2-second repetitions, concurrency 8. Follow modes include Go plus Rust and active ingestion. Every result validated. Strict native mode performs two identity/head batch barriers per eligible query. No production or block-execution throughput claim.'}
    save(out/'summary.json',report);save(ROOT/'results/latest-fresh-benchmark.json',{'directory':str(out.relative_to(ROOT))})
    print(json.dumps({k:v for k,v in report.items() if k in ['run','validated_calls','failed_requests','medians']},indent=2))


if __name__=='__main__':main()
