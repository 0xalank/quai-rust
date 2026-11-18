#!/usr/bin/env python3
"""Fixed Rust-only paired in-process handler/allocation experiment."""
import argparse,datetime,json,math,os
from pathlib import Path
import statistics,subprocess
from lab import ROOT,save,sha
p=argparse.ArgumentParser();p.add_argument('--baseline',required=True);p.add_argument('--candidate',required=True);p.add_argument('--rounds',type=int,default=3);p.add_argument('--iterations',type=int,default=20000);a=p.parse_args()
if not 3<=a.rounds<=10 or not 100<=a.iterations<=1000000:raise SystemExit('Use 3..10 rounds and 100..1000000 iterations')
binaries={k:Path(v).resolve() for k,v in [('baseline',a.baseline),('candidate',a.candidate)]}
cases=ROOT/'fixtures/performance/rpc-reads.json';snapshot=ROOT/'fixtures/native/scenario-a.json'
names=[v['name'] for v in json.loads(cases.read_text())]
pinned={str(x):sha(x) for x in [*binaries.values(),cases,snapshot,Path(__file__),ROOT/'crates/quai-rpc/examples/profile-rpc.rs']}
sources={str(x.relative_to(ROOT)):sha(x) for x in sorted((ROOT/'crates').rglob('*.rs'))}
accepted=json.loads((ROOT/'results/latest-rust.json').read_text());assert accepted['passed']
build=json.loads((ROOT/'results/rust-build.json').read_text())
for name,digest in build['rust_sources'].items():assert sha(ROOT/name)==digest
acceptance=json.loads((ROOT/accepted['directory']/'summary.json').read_text());assert acceptance['build_sha256']==sha(ROOT/'results/rust-build.json')
run=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')+'-handler-profile';out=ROOT/'results'/run;out.mkdir()
contract={'pinned_sha256':pinned,'candidate_sources':sources,'rounds':a.rounds,'iterations':a.iterations,'order':'alternating baseline/candidate each round','gate':'Every workload has fewer allocation calls, geometric mean handler rate improves at least 3%, and no workload median time regresses more than 5%. Every response validates. This is not an HTTP throughput promotion claim.','scope':'Instrumented Rust-only in-process example; independent timing and allocation passes; warm small frozen view. No sockets, Go service, external load client, EVM or sync.'}
save(out/'contract.json',contract);records=[]
for number in range(1,a.rounds+1):
 for mode in (['baseline','candidate'] if number%2 else ['candidate','baseline']):
  print(f'handler round {number}/{a.rounds}: {mode}',flush=True)
  result=subprocess.run([str(binaries[mode]),str(snapshot),str(cases),str(a.iterations)],capture_output=True,text=True,timeout=60)
  if result.returncode:raise RuntimeError(result.stderr+result.stdout)
  values=json.loads(result.stdout);assert values['passed'] and [v['name'] for v in values['workloads']]==names
  record={'mode':mode,'round':number,'result':values,'load_average':os.getloadavg()};records.append(record);save(out/f'{number}-{mode}.json',record)
comparisons=[]
for name in names:
 by={}
 for mode in binaries:
  rows=[v for r in records if r['mode']==mode for v in r['result']['workloads'] if v['name']==name]
  by[mode]={k:statistics.median(v[k] for v in rows) for k in ['ns_per_request','allocation_calls_per_request','gross_allocated_bytes_per_request']}
 ratio=by['baseline']['ns_per_request']/by['candidate']['ns_per_request']
 comparisons.append({'name':name,**by,'handler_rate_ratio':ratio,'allocation_guard':by['candidate']['allocation_calls_per_request']<by['baseline']['allocation_calls_per_request'],'time_guard':ratio>=1/1.05})
rate=math.exp(statistics.mean(math.log(v['handler_rate_ratio']) for v in comparisons))
for path,digest in pinned.items():assert sha(Path(path))==digest
for path,digest in sources.items():assert sha(ROOT/path)==digest
report={'passed':True,'run':run,'validated_requests':sum(v['validated_calls'] for r in records for v in r['result']['workloads']),'failed_requests':0,'geomean_handler_rate_ratio':rate,'promote_handler_change':rate>=1.03 and all(v['allocation_guard'] and v['time_guard'] for v in comparisons),'comparisons':comparisons,'records':records,'contract':contract,'acceptance_report':accepted['directory']+'/summary.json'}
save(out/'summary.json',report);save(ROOT/'results/latest-handler-profile.json',{'directory':str(out.relative_to(ROOT))})
print(json.dumps({k:v for k,v in report.items() if k not in ['records','contract']},indent=2))
