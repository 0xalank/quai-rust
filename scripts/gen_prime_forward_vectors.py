#!/usr/bin/env python3
"""Explicit offline exact-branch prime conversion oracle."""
import argparse,hashlib,json,os,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha

def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--go-quai',required=True)
 mode=p.add_mutually_exclusive_group(required=True);mode.add_argument('--write',action='store_true');mode.add_argument('--check',action='store_true');a=p.parse_args()
 go=Path(a.go_quai).resolve();checked_reference(go)
 source=go/'core/slice.go';text=source.read_text()
 assert sha(source)=='26f207c053321734a2cc33631b4039b66686f36ef16d86178e15dbd920d2cf1f'
 start=text.index('\tif nodeCtx == common.PRIME_CTX {\n\t\t// Until the controller has kicked in')
 stop=text.index('\n\ttime5 :=',start);branch=text[start:stop]
 pstart=text.index('func (sl *Slice) verifyParentExchangeRateAndFlowAmount(')
 pstop=text.index('\nfunc ',pstart);parent=text[pstart:pstop]
 old='func (sl *Slice) verifyParentExchangeRateAndFlowAmount('
 new='func (sl *Slice) NativePrimeSaveParentChoicesProbe('
 anchor='\t\t// ask the zone for this k quai, need to change the hash that is used to request this info'
 extra='\t\tif err := rawdb.WriteTokenChoicesSet(sl.sliceDb, parent.Hash(), &updatedTokenChoiceSet); err != nil { return err }\n'
 assert parent.count(old)==parent.count(anchor)==1
 probe=parent.replace(old,new,1).replace(anchor,extra+anchor,1)
 assert probe.replace(new,old,1).replace(extra,'',1)==parent
 template=ROOT/'reference/prime-forward/core-template.go';body=template.read_text()
 assert body.count('@@APPEND_BRANCH@@')==body.count('@@PARENT_PROBE@@')==1
 rendered=body.replace('@@APPEND_BRANCH@@',branch).replace('@@PARENT_PROBE@@',probe)
 out=ROOT/'results';out.mkdir(exist_ok=True);generated=out/'prime-forward-reference-core.go';generated.write_text(rendered)
 names=['reference/prime-forward/main.go','reference/prime-forward/core-template.go','reference/prime-forward/LICENSE','reference/prime-forward/NOTICE.md','results/prime-forward-reference-core.go']
 env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
 with tempfile.TemporaryDirectory(prefix='quai-prime-forward-oracle-')as directory:
  d=Path(directory);overlay=d/'overlay.json';binary=d/'oracle'
  overlay.write_text(json.dumps({'Replace':{str(go/'cmd/quai-prime-forward-vectors/main.go'):str(ROOT/names[0]),str(go/'core/native_prime_forward_vectors.go'):str(generated)}}))
  with(out/'prime-forward-oracle-build.log').open('w')as log:
   subprocess.run(['go','build','-overlay',str(overlay),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-prime-forward-vectors'],cwd=go,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
  result=subprocess.run([str(binary)],cwd=d,env=env,capture_output=True)
  (out/'prime-forward-oracle.stdout').write_bytes(result.stdout);(out/'prime-forward-oracle.stderr').write_bytes(result.stderr);result.check_returncode();value=json.loads(result.stdout)
 assert value['reference_revision']==PIN and len(value['vectors'])==256
 assert value['forward_steps']==1024 and all(len(v['steps'])==4 for v in value['vectors'])
 assert value['go_field_mutations_rejected']==768 and value['go_missing_history_rejected']==512
 assert value['undefined_zero_probe_cases']==240
 corpus=ROOT/'fixtures/protocol/prime-forward.json';encoded=(json.dumps(value,indent=2,sort_keys=True)+'\n').encode()
 sources=['core/slice.go','core/exchange_controller.go','consensus/misc/rewards.go','common/big.go','core/headerchain.go','params/protocol_params.go','core/bodydb.go','core/types/block.go','core/types/wo.go','go.mod','go.sum']
 digest=lambda s:hashlib.sha256(s.encode()).hexdigest()
 evidence={'reference_revision':PIN,'driver_sha256':sha(Path(__file__)),'oracle_sources':{n:sha(ROOT/n)for n in names},'sources':{n:sha(go/n)for n in sources},'scope':value['scope'],'runtime_go_dependency':False,'canonical_authority':False,'cases':256,'linked_forward_steps':1024,'undefined_zero_probe_cases':240,'append_branch_range_chars':[start,stop],'append_branch_sha256':digest(branch),'append_branch_byte_identical':True,'parent_probe_range_chars':[pstart,pstop],'parent_original_sha256':digest(parent),'parent_probe_sha256':digest(probe),'parent_probe_changes':['Rename the method','One test-only token-choice write to supply the initial parent seed'],'unmodified_parent_verifier_also_executed':True}
 if a.write:corpus.write_bytes(encoded)
 else:assert corpus.read_bytes()==encoded,'Prime forward corpus differs'
 evidence['vectors_sha256']=sha(corpus);meta=corpus.with_name('prime-forward-source.json')
 if a.write:meta.write_text(json.dumps(evidence,indent=2)+'\n')
 else:assert json.loads(meta.read_text())==evidence,'Prime forward source differs'
 print(json.dumps({'passed':True,'cases':256,'linked_forward_steps':1024,'undefined_zero_probes':240,'canonical_authority':False}))
if __name__=='__main__':main()
