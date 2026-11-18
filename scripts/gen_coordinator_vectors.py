#!/usr/bin/env python3
"""Explicit offline coordinator oracle with observation-only source hooks."""
import argparse,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def instrument(source):
    a=source.index('func (hc *HierarchicalCoordinator) PendingHeadersMap()');b=source.index('func (hc *HierarchicalCoordinator) NodeFromHash',a);body=source[a:b]
    changes=[('// Pick the leader among all the slices','csAttempt()\n    // Pick the leader among all the slices'),('leaders = hc.CalculateLeaders(badHashes)','leaders = hc.CalculateLeaders(badHashes)\n    csLeaders(leaders)'),('modifiedConstraintMap, err = hc.calculateFrontierPoints(modifiedConstraintMap, leaderBlock, first)','wasFirst := first\n            modifiedConstraintMap, err = hc.calculateFrontierPoints(modifiedConstraintMap, leaderBlock, first)\n            csTrace(location, node.hash, wasFirst, err)'),('primeTermini := hc.GetBackend','csConstraints(modifiedConstraintMap)\n    primeTermini := hc.GetBackend'),('badHashes[modifiedConstraintMap[regionLocation]] = true','badHashes[modifiedConstraintMap[regionLocation]] = true\n            csReject(modifiedConstraintMap[regionLocation])'),('badHashes[modifiedConstraintMap[zoneLocation]] = true','badHashes[modifiedConstraintMap[zoneLocation]] = true\n                csReject(modifiedConstraintMap[zoneLocation])')]
    for old,new in changes:
        assert body.count(old)==1,old
        body=body.replace(old,new)
    source=source[:a]+body+source[b:]
    a=source.index('func (hc *HierarchicalCoordinator) BuildPendingHeaders(');b=source.index('func (hc *HierarchicalCoordinator) ComputePendingHeaders(',a);body=source[a:b]
    for old,new in [('startingLen := len(hc.pendingHeaders.order)','csApplyStart()\n    startingLen := len(hc.pendingHeaders.order)'),('entropy = hc.pendingHeaders.order[i]','entropy = hc.pendingHeaders.order[i]\n        csVisits++'),('if nodeSet.Extendable(wo, order) {','if nodeSet.Extendable(wo, order) {\n            csExtensions++')]:
        assert body.count(old)==1,old
        body=body.replace(old,new)
    source=source[:a]+body+source[b:]
    old='time.Sleep(time.Duration(sleepTime) * time.Second)';assert source.count(old)==1;return source.replace(old,'csSleep(time.Duration(sleepTime) * time.Second)')

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True);m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args();go=Path(a.go_quai).resolve();checked_reference(go)
    sources=['reference/coordinator/main.go','reference/coordinator/coordinator-export.go','reference/frontier/coordinator-export.go','reference/eligibility/coordinator-export.go'];env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
    goroot=Path(subprocess.check_output(['go','env','GOROOT'],env=env,text=True).strip())
    with tempfile.TemporaryDirectory(prefix='quai-coordinator-oracle-') as directory:
        d=Path(directory);modified=d/'hierarchical_coordinator.go';modified.write_text(instrument((go/'cmd/utils/hierarchical_coordinator.go').read_text()));overlay=d/'overlay.json';binary=d/'oracle';overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-coordinator-vectors/main.go'):str(ROOT/sources[0]),str(go/'cmd/utils/native_coordinator_vectors.go'):str(ROOT/sources[1]),str(go/'cmd/utils/native_frontier_vectors.go'):str(ROOT/sources[2]),str(go/'cmd/utils/native_eligibility_vectors.go'):str(ROOT/sources[3]),str(go/'cmd/utils/hierarchical_coordinator.go'):str(modified)}}))
        instrumented_hash=sha(modified);subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-coordinator-vectors'],cwd=go,env=env,check=True)
        value=json.loads(subprocess.check_output([str(binary),str(ROOT/'fixtures/protocol/eligibility.json')],cwd=d,env=env))
    assert value['reference_revision']==PIN
    policy=value['policy'];native=value['native']
    assert {k:len(policy[k]) for k in ['datasets','searches','sorts','events','leader_boundaries','reapplications','sleeps']}=={'datasets':24,'searches':120,'sorts':135,'events':2,'leader_boundaries':2,'reapplications':30,'sleeps':7}
    assert sum(len(row['steps']) for row in policy['events'])==2044
    assert sum(len(row['steps']) for row in policy['reapplications'])==90
    assert max(step['visited'] for row in policy['reapplications'] for step in row['steps'])==21
    assert sum(row['result']['accepted'] for row in policy['searches'])==40
    assert all(len(row['result']['attempts'])==3 for row in policy['searches'] if not row['result']['accepted'])
    assert len(native['searches'])==16 and all(row['result']['accepted'] for row in native['searches'])
    assert len(native['events'])==9 and sum(len(row['steps']) for row in native['reapplications'])==12
    path=ROOT/'fixtures/protocol/coordinator.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
    evidence={'schema':1,'reference_revision':PIN,'oracle_sources':{n:sha(ROOT/n) for n in sources},'driver_sha256':sha(Path(__file__).resolve()),'input_eligibility_sha256':sha(ROOT/'fixtures/protocol/eligibility.json'),'input_eligibility_source_sha256':sha(ROOT/'fixtures/protocol/eligibility-source.json'),'sources':{n:sha(go/n) for n in ['cmd/utils/hierarchical_coordinator.go','common/types.go','go.mod','go.sum']},'instrumented_source_sha256':instrumented_hash,'toolchain_sort_sources':{n:sha(goroot/n) for n in ['src/sort/zsortfunc.go','src/sort/sort.go','src/sort/slice.go']},'go_version':subprocess.check_output(['go','version'],env=env,text=True).strip(),'observation_only_search_hooks':True,'virtual_clock':True,'worker_dispatch_suppressed':True,'public_network':False,'runtime_go_dependency':False,'scope':value['scope']}
    if a.write:path.write_bytes(encoded)
    elif path.read_bytes()!=encoded:raise SystemExit('Coordinator corpus differs from pinned reference')
    evidence['vectors_sha256']=sha(path);target=ROOT/'fixtures/protocol/coordinator-source.json'
    if a.write:target.write_text(json.dumps(evidence,indent=2)+'\n')
    elif json.loads(target.read_text())!=evidence:raise SystemExit('Coordinator source evidence differs')
    print(json.dumps({'passed':True,'mode':'write' if a.write else 'check','policy':{k:len(v) for k,v in value['policy'].items()},'native_searches':len(value['native']['searches'])}))
if __name__=='__main__':main()
