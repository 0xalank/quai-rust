#!/usr/bin/env python3
"""Matched offline transaction/signature pipeline benchmark, with fixed correctness."""
import argparse,contextlib,datetime,hashlib,json,math,os
from pathlib import Path
import selectors,signal,statistics,subprocess,sys,tempfile,time
p=argparse.ArgumentParser();p.add_argument('--stage',required=True);p.add_argument('--project',default='/Users/comp/code/soap/quai-rust');a=p.parse_args()
s=Path(a.stage).resolve();root=Path(a.project).resolve()
sys.path.insert(0,str(root/'scripts'));from benchmark_native import Sampler
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
def save(path,value):path.parent.mkdir(parents=True,exist_ok=True);path.write_text(json.dumps(value,indent=2)+'\n')
build=json.loads((s/'results/build.json').read_text())
for name,b in build['binaries'].items():
 if b['available']:assert sha(s/'bin'/name)==b['sha256'],name
for path,digest in build['sources'].items():assert sha(s/path)==digest,path
corpus=s/'fixtures/protocol/transactions.json';assert sha(corpus)==build['corpus_sha256']
run=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')+'-transaction-pipeline';out=s/'results'/run;out.mkdir()
(s/'results/latest.json').write_text(json.dumps({'directory':str(out.relative_to(s))},indent=2)+'\n')
class Program:
 def __init__(self,name):self.name=name;self.request_count=0
 def __enter__(self):
  self.log=tempfile.TemporaryFile(mode='w+')
  self.process=subprocess.Popen([str(s/'bin'/self.name),str(corpus)],cwd=s,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.log,text=True,bufsize=1,env={**os.environ,'GOGC':'100','GOMEMLIMIT':'off'})
  try:self.ready=self.read(60);assert self.ready['ready'] and self.ready['preflight']['passed'];return self
  except BaseException:self.__exit__(None,None,None);raise
 def read(self,timeout):
  with selectors.DefaultSelector() as selector:
   selector.register(self.process.stdout,selectors.EVENT_READ)
   if not selector.select(timeout):raise RuntimeError(self.name+' command timeout')
  line=self.process.stdout.readline()
  if not line:
   self.log.seek(0);raise RuntimeError(self.name+' exited: '+self.log.read())
  return json.loads(line)
 def command(self,value):
  self.process.stdin.write(json.dumps(value)+'\n');self.process.stdin.flush();self.request_count+=1
  result=self.read(120);assert result['passed'],result;return result
 def __exit__(self,*_):
  if getattr(self,'process',None):
   if self.process.poll() is None:
    try:self.process.stdin.write('{"quit":true}\n');self.process.stdin.flush();self.process.wait(timeout=5)
    except (BrokenPipeError,subprocess.TimeoutExpired):
     self.process.terminate()
     try:self.process.wait(timeout=5)
     except subprocess.TimeoutExpired:self.process.kill();self.process.wait()
   for stream in [self.process.stdin,self.process.stdout]:
    if stream:stream.close()
  if hasattr(self,'log'):self.log.close()
primary=['quai_full','qi_single_full','qi_multi_full','qi_invalid_full']
stages=['quai_decode','qi_decode','quai_signing_hash','qi_signing_hash','qi_aggregate']
languages=['go-cgo','rust']
if build['binaries']['go-pure']['available']:languages.insert(1,'go-pure')
pinned={str(x):sha(x) for x in [corpus,s/'fixtures/protocol/transactions-source.json',s/'results/build.json',Path(__file__),root/'scripts/benchmark_native.py',root/'scripts/lab.py']}
contract={'schema':1,'reference_revision':build['reference_revision'],'primary_workloads':primary,'stage_workloads':stages,'workers':{'primary':[1,4],'stages':[1]},'languages':languages,'rounds':5,'allocation_rounds':3,'allocation_iterations':512,'iterations_rule':'Calibrate all languages at 512 operations for full pipelines and 4096 for stages. Common per-workload count is the faster calibration rate times 1.0 seconds (full) or 0.35 seconds (stage), rounded up to 64; clamp to 1024..32768 (full) or 1024..1048576 (stage). Same count for both worker counts and every implementation.','timing_order':'Reverse case order on alternate rounds and alternate implementation order per round/case. Only one workload under load at a time.','timing_instrumentation':'Rust timing binary uses the default allocator without the counting wrapper. Rust allocation binary is separately compiled. Go uses standard runtime counters in allocation-only samples.','correctness':'Frozen corpus preflight on process startup; each timed operation checks the result, signing digest, decoded type or aggregate key for its declared stage; compare checksum/count across implementations. Full operations always decode fresh bytes. Go signers prepared outside timing; sender cache cannot bypass fresh full operations.','scope':'Small warm synthetic corpus. Full means decode plus signature pipeline, not UTXO ownership, state/fee/lock validation, EVM, full blocks, RPC, disk, P2P or sync. Allocation metrics cover tracked heap requests; Go counters exclude C allocations. Rust is pure k256; Go uses the pinned production CGO backend.','source_sha256':pinned}
save(out/'contract-before-calibration.json',contract)
records=[];calibrations=[];allocations=[]
with contextlib.ExitStack() as stack:
 programs={name:stack.enter_context(Program(name)) for name in [*languages,'rust-alloc']}
 groups=programs['rust'].ready['groups']
 for name,program in programs.items():assert program.ready['groups']==groups,(name,program.ready)
 assert not programs['rust'].ready['allocation_instrumented']
 assert programs['rust-alloc'].ready['allocation_instrumented']
 save(out/'preflight.json',{name:program.ready for name,program in programs.items()})
 counts={}
 for workload in primary+stages:
  print('Calibrating',workload,flush=True)
  values=[];n=512 if workload in primary else 4096
  for language in languages:
   result=programs[language].command({'workload':workload,'workers':1,'iterations':n,'measure':'time'})
   values.append(result['ns_per_transaction']);calibrations.append({'language':language,**result})
  target=1.0 if workload in primary else .35;cap=32768 if workload in primary else 1048576
  counts[workload]=min(cap,max(1024,math.ceil(target*1e9/min(values)/64)*64))
 contract['iterations']=counts;contract['groups']=groups
 save(out/'contract.json',contract);save(out/'calibration.json',calibrations)
 cases=[(w,n) for w in primary for n in [1,4]]+[(w,1) for w in stages]
 for number in range(1,6):
  order=cases if number%2 else list(reversed(cases))
  for case_number,(workload,workers) in enumerate(order):
   order_languages=languages if (number+case_number)%2 else list(reversed(languages))
   pair=[]
   for language in order_languages:
    print(f'round {number}/5 {workload} workers={workers}: {language}',flush=True)
    program=programs[language]
    before_load=os.getloadavg()
    with Sampler([program.process.pid]) as resources:
     result=program.command({'workload':workload,'workers':workers,'iterations':counts[workload],'measure':'time'})
    assert result['validated_transactions']==counts[workload]
    record={'language':language,'round':number,'workload':workload,'workers':workers,**result,'server_cpu_seconds':resources.cpu,'sampled_peak_rss_kib':resources.peak,'load_before':before_load,'load_after':os.getloadavg()}
    records.append(record);pair.append(record)
    save(out/'timing'/f'{number}-{workload}-{workers}-{language}.json',record)
   assert len({(v['checksum'],v['validated_transactions']) for v in pair})==1,(workload,pair)
 # Allocation passes are sequential and separate from timing.
 for number in range(1,4):
  for workload in primary+stages:
   checksum=[]
   for language in languages:
    program=programs['rust-alloc' if language=='rust' else language]
    result=program.command({'workload':workload,'workers':1,'iterations':512,'measure':'alloc'})
    row={'language':language,'round':number,**result};allocations.append(row);checksum.append(row['checksum'])
    save(out/'allocations'/f'{number}-{workload}-{language}.json',row)
   assert len(set(checksum))==1
 for language in programs:save(out/'processes'/f'{language}.json',{'pid':programs[language].process.pid,'commands':programs[language].request_count})
medians=[]
for workload,workers in cases:
 for language in languages:
  group=[v for v in records if (v['workload'],v['workers'],v['language'])==(workload,workers,language)]
  assert len(group)==5
  allocation=[v for v in allocations if v['workload']==workload and v['language']==language]
  medians.append({'workload':workload,'workers':workers,'language':language,'ns_per_transaction':statistics.median(v['ns_per_transaction'] for v in group),'transactions_per_second':statistics.median(v['transactions_per_second'] for v in group),'min_ns':min(v['ns_per_transaction'] for v in group),'max_ns':max(v['ns_per_transaction'] for v in group),'server_cpu_us_per_transaction':statistics.median(v['server_cpu_seconds']*1e6/v['iterations'] for v in group),'sampled_peak_rss_kib':max(v['sampled_peak_rss_kib'] for v in group),'allocation_calls_per_transaction':statistics.median(v['allocation_calls_per_transaction'] for v in allocation),'gross_allocated_bytes_per_transaction':statistics.median(v['gross_allocated_bytes_per_transaction'] for v in allocation)})
comparisons=[]
for workload,workers in cases:
 go=next(v for v in medians if (v['workload'],v['workers'],v['language'])==(workload,workers,'go-cgo'))
 rust=next(v for v in medians if (v['workload'],v['workers'],v['language'])==(workload,workers,'rust'))
 pairs=[]
 for number in range(1,6):
  by={v['language']:v for v in records if v['workload']==workload and v['workers']==workers and v['round']==number}
  pairs.append(by['go-cgo']['ns_per_transaction']/by['rust']['ns_per_transaction'])
 comparisons.append({'workload':workload,'workers':workers,'rust_over_go_rate':rust['transactions_per_second']/go['transactions_per_second'],'median_paired_rate':statistics.median(pairs),'paired_rate_range':[min(pairs),max(pairs)],'paired_rates':pairs})
for path,digest in pinned.items():assert sha(Path(path))==digest,path
for path,digest in build['sources'].items():assert sha(s/path)==digest,path
for name,b in build['binaries'].items():
 if b['available']:assert sha(s/'bin'/name)==b['sha256']
before=json.loads((s/'results/shipping-before.json').read_text())
for n,h in before['binary_hashes'].items():assert sha(root/'bin'/n)==h,n
for n,h in before['build']['rust_sources'].items():assert sha(root/n)==h,n
summary={'passed':True,'run':run,'timed_samples':len(records),'allocation_samples':len(allocations),'timed_operations_validated':sum(v['validated_transactions'] for v in records),'allocation_operations_validated':sum(v['validated_transactions'] for v in allocations),'calibration_operations_validated':sum(v['validated_transactions'] for v in calibrations),'failed_operations':0,'medians':medians,'comparisons':comparisons,'contract':contract,'build':build,'shipping_sources_and_binaries_unchanged':True,'scope':contract['scope']}
save(out/'summary.json',summary)
print(json.dumps({k:v for k,v in summary.items() if k in ['passed','run','timed_samples','allocation_samples','timed_operations_validated','allocation_operations_validated','failed_operations','comparisons']},indent=2))
