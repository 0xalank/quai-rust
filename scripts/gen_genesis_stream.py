#!/usr/bin/env python3
"""Explicit offline Go oracle: bounded genesis-linked JSONL stream, no checkpoint."""
import argparse,datetime,gzip,hashlib,json,os,shutil,subprocess,tempfile
from pathlib import Path
from gen_foundation_vectors import ROOT,PIN,checked_reference,sha
p=argparse.ArgumentParser();p.add_argument('--go-quai',type=Path,required=True);p.add_argument('--blocks',type=int,required=True);p.add_argument('--out',type=Path,required=True);a=p.parse_args();assert 1<=a.blocks<=300000;go=a.go_quai.resolve();checked_reference(go);out=a.out.resolve();assert out.is_relative_to(ROOT/'results')and not out.exists();out.mkdir(parents=True)
helpers=[ROOT/'reference/genesis-stream/main.go',ROOT/'reference/genesis-stream/core-export.go',ROOT/'reference/confirmation/harness.go'];driver=sha(Path(__file__));sources={str(x.relative_to(ROOT)):sha(x)for x in helpers};profile=ROOT/'configs/private-execution-v2.json';profile_hash=sha(profile);shutil.copy2(Path(__file__),out/'oracle-driver.py')
env={**os.environ,'GOPROXY':'off','GOTOOLCHAIN':'local'}
with tempfile.TemporaryDirectory(prefix='quai-genesis-stream-build-')as directory:
 d=Path(directory);overlay={'Replace':{str(go/'cmd/quai-genesis-stream/main.go'):str(helpers[0]),str(go/'core/native_genesis_stream.go'):str(helpers[1]),str(go/'core/native_genesis_harness.go'):str(helpers[2])}};(d/'overlay.json').write_text(json.dumps(overlay));binary=d/'oracle'
 with(out/'build.log').open('w')as f:subprocess.run(['go','build','-overlay',str(d/'overlay.json'),'-ldflags','-X main.referenceRevision='+PIN,'-o',str(binary),'./cmd/quai-genesis-stream'],cwd=go,env=env,stdout=f,stderr=subprocess.STDOUT,check=True)
 binary_hash=sha(binary);raw_hash=hashlib.sha256();count=0;initial=None;final=None;pending=out/'blocks.jsonl.gz.pending'
 with(out/'oracle.stderr').open('w')as err,pending.open('wb')as disk:
  proc=subprocess.Popen([str(binary),str(a.blocks)],cwd=d,env=env,stdout=subprocess.PIPE,stderr=err)
  try:
   with gzip.GzipFile(filename='',mode='wb',compresslevel=1,fileobj=disk,mtime=0)as compressed:
    for line in proc.stdout:
     row=json.loads(line);raw_hash.update(line);compressed.write(line)
     if row['kind']=='genesis':assert initial is None and count==0 and row['blocks']==a.blocks and row['reference_revision']==PIN and not row['checkpoint_imported'];initial=row
     elif row['kind']=='block':
      count+=1;assert initial is not None and final is None and row['height']==count and row['signed_quai_transactions']==max(0,count-259200)
      if count%1000==0:print(json.dumps({'height':count,'signed_quai_transactions':row['signed_quai_transactions']}),flush=True)
     elif row['kind']=='complete':assert count==a.blocks and row['blocks']==a.blocks and row['validated_from_genesis']and not row['checkpoint_imported'];final=row
     else:raise AssertionError('Unknown stream record')
   code=proc.wait();assert code==0,(code,(out/'oracle.stderr').read_text()[-2000:]);assert initial is not None and final is not None
  finally:
   if proc.poll()is None:proc.kill();proc.wait()
   proc.stdout.close()
 pending.rename(out/'blocks.jsonl.gz')
assert driver==sha(Path(__file__))and profile_hash==sha(profile)
for n,h in sources.items():assert sha(ROOT/n)==h,n
meta={'passed':True,'reference_revision':PIN,'blocks':count,'signed_quai_transactions':final['signed_quai_transactions'],'head':final['head'],'now':initial['now'],'genesis_hash':initial['hash'],'validated_from_genesis':True,'checkpoint_imported':False,'initial_allocations':False,'public_network':False,'runtime_go_dependency':False,'global_consensus_qualified':False,'profile_sha256':profile_hash,'driver_sha256':driver,'oracle_binary_sha256':binary_hash,'oracle_sources':sources,'raw_stream_sha256':raw_hash.hexdigest(),'compressed_stream_sha256':sha(out/'blocks.jsonl.gz'),'completed_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'go_sources':{n:sha(go/n)for n in ['core/headerchain_validation.go','core/headerchain.go','core/block_validator.go','core/state_processor.go','core/state_transition.go','core/slice.go','core/state/statedb.go','core/state/state_object.go','core/rawdb/accessors_chain.go','core/rawdb/accessors_state.go','core/types/transaction_signing.go','core/types/utxo.go','core/evm.go','core/types/wo.go','consensus/progpow/progpow.go','consensus/progpow/algorithm.go','consensus/progpow/algorithm_progpow.go','consensus/misc/statefee.go','consensus/misc/rewards.go','params/protocol_params.go','go.mod','go.sum']},'scope':'Explicit offline genesis-to-height native Go AppendHeader/ValidateBody/StateProcessor/ValidateState/PCRC/confirmation oracle on a fixed private profile. No supplied checkpoint or state allocation. Signed Quai self-transfers after zone transaction activation spend only rewards matured by the unmodified reference. Other ETX kinds, later forks, nonlocal/expansion and full public canonical consensus remain outside this corpus.'}
(out/'source.json').write_text(json.dumps(meta,indent=2)+'\n');print(json.dumps({'passed':True,'blocks':count,'signed_quai_transactions':meta['signed_quai_transactions'],'head':meta['head'],'directory':str(out)}),flush=True)
