#!/usr/bin/env python3
"""Finite UCB1 configuration search with correctness and holdout gates."""
import argparse
import datetime
import json
import math
import os
import platform
import statistics
from lab import BASELINE,ROOT,Server,bench,check_fixture,save,sha,verify_contract

ARMS=[
    ('baseline',BASELINE),
    ('raw',dict(BASELINE,encoding='raw')),
    ('parallel4',dict(BASELINE,batch_concurrency=4)),
    ('raw-parallel4',dict(BASELINE,encoding='raw',batch_concurrency=4)),
    ('raw-parallel8',dict(BASELINE,encoding='raw',batch_concurrency=8)),
    ('raw-parallel8-workers4',dict(BASELINE,encoding='raw',batch_concurrency=8,workers=4)),
]

def measure(config,profiles,oracle=None,go=False,seed=20261103):
    with Server('go' if go else 'rust',config=config) as server:
        checks=0 if go else check_fixture(server.url,oracle,seed=seed,generated=12)
        values=[bench(server.url,profile) for profile in profiles]
        return {'config':config,'checks':checks,'workloads':values,'sampled_peak_rss_kib':server.peak_rss_kib}

def compare(baseline,candidate):
    comparisons=[]
    for a,b in zip(baseline['workloads'],candidate['workloads']):
        assert a['name']==b['name']
        ratio=b['calls_per_second']/a['calls_per_second']
        # This is a coarse regression guard for exploratory shared-host runs,
        # not a production latency SLA. Both measurements are preserved.
        latency_ok=b['request_p95_ms']<=2*a['request_p95_ms']+5
        comparisons.append({'name':a['name'],'throughput_ratio':ratio,
            'baseline_p95_ms':a['request_p95_ms'],'candidate_p95_ms':b['request_p95_ms'],
            'latency_guard_passed':latency_ok})
    memory_ok=0<candidate['sampled_peak_rss_kib']<=256*1024
    passed=memory_ok and all(x['latency_guard_passed'] for x in comparisons)
    ratio=math.exp(statistics.mean(math.log(x['throughput_ratio']) for x in comparisons))
    reward=max(-2,min(2,math.log(ratio))) if passed else -2
    return {'geomean_throughput_ratio':ratio,'reward':reward,'gates_passed':passed,
            'memory_guard_passed':memory_ok,'workloads':comparisons}

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--trials',type=int,default=8)
    args=parser.parse_args()
    if not 6<=args.trials<=60:raise SystemExit('Use 6–60 trials; initial six trials cover all configurations')
    contract=verify_contract()
    build=json.loads((ROOT/'results/build.json').read_text())
    for binary,digest in build['binaries'].items():
        if sha(ROOT/'bin'/binary)!=digest:raise SystemExit('Rebuild before tuning: binary changed')
    for name,digest in build['rust_sources'].items():
        if sha(ROOT/name)!=digest:raise SystemExit('Rebuild before tuning: Rust source changed')
    fixture=json.loads((ROOT/'fixtures/workloads.json').read_text())
    run_id=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    out=ROOT/'results'/run_id;out.mkdir(parents=True,exist_ok=False)
    save(out/'environment.json',{'started_utc':run_id,'platform':platform.platform(),
        'load_average':os.getloadavg(),'contract':contract,'build':build,
        'policy':'UCB1 over six fixed configurations; finite multi-armed bandit, no neural-model training',
        'score':'mean log throughput ratio vs paired Rust baseline; all correctness checks required; coarse p95 and sampled-RSS guards'})
    counts=[0]*len(ARMS);rewards=[0.0]*len(ARMS);records=[]
    with Server('go') as oracle:
        for step in range(args.trials):
            index=step if step<len(ARMS) else max(range(len(ARMS)),key=lambda i:rewards[i]/counts[i]+math.sqrt(2*math.log(step+1)/counts[i]))
            name,config=ARMS[index]
            print(f'trial {step+1}/{args.trials}: {name}',flush=True)
            try:
                if step%2:
                    candidate=measure(config,fixture['train'],oracle.url)
                    baseline=measure(BASELINE,fixture['train'],oracle.url)
                else:
                    baseline=measure(BASELINE,fixture['train'],oracle.url)
                    candidate=measure(config,fixture['train'],oracle.url)
                comparison=compare(baseline,candidate)
                record={'run':run_id,'trial':step+1,'arm':name,'baseline':baseline,'candidate':candidate,'comparison':comparison,'load_average':os.getloadavg()}
                reward=comparison['reward']
            except Exception as error:
                record={'run':run_id,'trial':step+1,'arm':name,'error':str(error),'comparison':{'reward':-2,'gates_passed':False}}
                reward=-2
            records.append(record);counts[index]+=1;rewards[index]+=reward
            save(out/f'trial-{step+1:02d}.json',record)
            with (ROOT/'autoresearch.jsonl').open('a') as log:log.write(json.dumps(record)+'\n')
            print(f'  reward={reward:.3f}, gates={record["comparison"]["gates_passed"]}',flush=True)
        best=max(range(len(ARMS)),key=lambda i:rewards[i]/counts[i])
        best_name,best_config=ARMS[best]
        print(f'holdout: {best_name}',flush=True)
        holdout=[]
        for round_number in range(2):
            if round_number%2:
                candidate=measure(best_config,fixture['holdout'],oracle.url,seed=84673)
                baseline=measure(BASELINE,fixture['holdout'],oracle.url,seed=84673)
            else:
                baseline=measure(BASELINE,fixture['holdout'],oracle.url,seed=84673)
                candidate=measure(best_config,fixture['holdout'],oracle.url,seed=84673)
            holdout.append({'round':round_number+1,'baseline':baseline,'candidate':candidate,'comparison':compare(baseline,candidate)})
        go_reference=measure(BASELINE,fixture['holdout'],go=True)
    promote=best_name!='baseline' and all(r['comparison']['gates_passed'] and r['comparison']['geomean_throughput_ratio']>1.03 for r in holdout)
    result={'run':run_id,'trials':args.trials,'policy':'UCB1 finite configuration bandit',
        'selected_arm':best_name,'candidate_config':best_config,'promoted':promote,
        'recommended_config':best_config if promote else BASELINE,
        'holdout':holdout,'go_reference':go_reference,
        'arm_statistics':[{'name':name,'trials':counts[i],'mean_reward':rewards[i]/counts[i]} for i,(name,_) in enumerate(ARMS)],
        'scope':'Synthetic HTTP fixture service and scheduling. No whole-node, real database/EVM, mainnet, TLS or WebSocket result.'}
    save(out/'summary.json',result)
    save(ROOT/'results/latest.json',{'run_directory':str(out.relative_to(ROOT)),'selected_arm':best_name,'promoted':promote})
    save(ROOT/'configs/candidate.json',best_config)
    save(ROOT/'configs/recommended.json',result['recommended_config'])
    print(json.dumps({'run':run_id,'selected_arm':best_name,'promoted':promote,
        'holdout_ratios':[r['comparison']['geomean_throughput_ratio'] for r in holdout]},indent=2))

if __name__=='__main__':main()
