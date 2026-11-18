#!/usr/bin/env python3
"""Explicit latest-source Go-handler/SDK check over localhost only."""
import argparse,json,os,selectors,subprocess,tempfile,datetime
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
out=ROOT/'results'/('upstream-sdk-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
with tempfile.TemporaryDirectory(prefix='quai-upstream-sdk-')as directory:
 d=Path(directory);binary=d/'oracle';overlay=d/'overlay.json';source=ROOT/'reference/scenarios/main.go';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-scenario-oracle/main.go'):str(source)}}))
 with (out/'build.log').open('w')as log:subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-scenario-oracle'],cwd=go,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
 with (out/'server.log').open('w')as log:
  process=subprocess.Popen([str(binary),'--version-dir',str(go),'--listen','127.0.0.1:0'],cwd=ROOT,env=env,stdout=subprocess.PIPE,stderr=log,text=True)
  try:
   with selectors.DefaultSelector()as selector:
    selector.register(process.stdout,selectors.EVENT_READ)
    if not selector.select(30):raise RuntimeError('Latest-source local oracle failed to start')
   line=process.stdout.readline()
   if not line:raise RuntimeError('Latest-source local oracle exited; see server.log')
   ready=json.loads(line);url='http://'+ready['address'];assert ready['address'].startswith('127.0.0.1:')
   from urllib.request import urlopen
   with urlopen(url+'/profile',timeout=10)as response:profile=json.load(response)
   assert profile['reference_revision']==PIN
   previous=json.loads((ROOT/'fixtures/protocol/sdk-profile.json').read_text());profile['topic']=previous['topic'];(out/'profile.json').write_text(json.dumps(profile,indent=2)+'\n')
   cases=0
   for alias in ['quais57','quais58']:
    target=out/(alias+'.json');r=subprocess.run(['node',str(ROOT/'sdk/fresh_acceptance.cjs'),alias,url,str(out/'profile.json'),str(target)],cwd=ROOT/'sdk',env=env,capture_output=True,text=True,timeout=60);(out/(alias+'.log')).write_text(r.stdout+r.stderr)
    if r.returncode:raise RuntimeError('Latest-source SDK call failed; see '+alias+'.log')
    actual=json.loads(target.read_text());expected=json.loads((ROOT/f'fixtures/protocol/{alias}-expected.json').read_text());assert actual==expected,alias+' output changed; review the new behavioral baseline';cases+=len(actual['results'])
   summary={'passed':True,'reference_revision':PIN,'sdk_comparisons':cases,'oracle_binary_sha256':sha(binary),'oracle_source_sha256':sha(source),'sdk_lock_sha256':sha(ROOT/'sdk/package-lock.json'),'public_network':False,'scope':'Actual official v0.57.0 Go handlers over disposable seeded state, checked through both pinned SDKs. Frozen expected values unchanged; no PoW/canonical validation or public network.'};(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');(ROOT/'results/latest-upstream-sdk.json').write_text(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT))},indent=2)+'\n')
   evidence=json.loads((ROOT/'fixtures/protocol/sdk-source.json').read_text());evidence['previous_reference_revision']=evidence['reference_revision'];evidence['reference_revision']=PIN;evidence['recorded_report']=str(out.relative_to(ROOT)/'summary.json');evidence['scope']=summary['scope'];(ROOT/'fixtures/protocol/sdk-source.json').write_text(json.dumps(evidence,indent=2)+'\n');(ROOT/'fixtures/protocol/sdk-profile.json').write_text(json.dumps(profile,indent=2)+'\n');print(json.dumps({'passed':True,'sdk_comparisons':cases,'directory':str(out)}))
  finally:
   process.terminate();process.wait(timeout=10)
