#!/usr/bin/env python3
"""Qualify the bounded private canonical path; optional real loopback peers."""
import argparse, datetime, json, os, shutil, subprocess, tempfile
from pathlib import Path
from lab import ROOT, sha, save

def main():
 p=argparse.ArgumentParser();p.add_argument('--peer',action='store_true');args=p.parse_args()
 build=json.loads((ROOT/'results/rust-build.json').read_text())
 for group in ['rust_sources','source_assets']:
  for name,digest in build[group].items():assert sha(ROOT/name)==digest,'Rebuild first: '+name
 assert build['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
 corpus=ROOT/'fixtures/protocol/canonical.json';evidence=corpus.with_name('canonical-source.json');m=json.loads(evidence.read_text())
 assert m['reference_revision']==build['reference_revision'] and m['vectors_sha256']==sha(corpus)
 for key,name in [('input_corpus_sha256','fixtures/protocol/weakshares.json'),('input_evidence_sha256','fixtures/protocol/weakshares-source.json'),('profile_sha256','configs/private-hierarchy-v1.json'),('driver_sha256','scripts/gen_canonical_vectors.py')]:assert m[key]==sha(ROOT/name),name
 for name,digest in m['oracle_sources'].items():assert sha(ROOT/name)==digest,name
 out=ROOT/'results'/('canonical-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir(parents=True)
 target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()
 def run(name,package,extra=()):
  with (out/(name+'-build.log')).open('w')as log:subprocess.run(['cargo','build','--release','--locked','--offline','-p',package,'--example',name,*extra],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
  binary=out/name;shutil.copy2(target/'release/examples'/name,binary)
  with tempfile.TemporaryDirectory(prefix='quai-private-canonical-')as directory:
   r=subprocess.run([str(binary),str(corpus),directory],cwd=ROOT,text=True,capture_output=True,timeout=180)
  (out/(name+'.log')).write_text(r.stdout+r.stderr);assert r.returncode==0,(name,r.stderr[-1000:])
  result=json.loads(r.stdout);assert result['passed'] and not result['global_full_consensus'] and not result['canonical_public_sync']
  save(out/(name+'.json'),result)
  return result,sha(binary)
 native,binary=run('check-canonical','quai-consensus')
 assert native['lazy_etx']and native['lazy_lockups']and native['lazy_accounts']and native['lazy_storage']and native['lazy_qi']and native['indexed_catalog']and native['indexed_ancestry']and native['canonical_execution_uses_state_database']and not native['complete_state_images_written']
 assert native['checks']==488 and native['events']==25 and native['unique_native_blocks']==24 and native['higher_entropy_shorter_branch_reorgs']==1 and native['restart_checks']==3 and native['invalid_candidates_rejected']==8
 peer=None;peer_binary=None
 if args.peer:
  peer,peer_binary=run('canonical-sync','quai-network',['--features','native-transport'])
  assert peer['unique_native_blocks_imported']==24 and peer['raw_object_requests']==38 and peer['receiver_restart_checks']==2 and peer['adversarial_peers_or_blocks_rejected']==4 and peer['canonical_reorgs']==1
  assert peer['noise'] and peer['yamux'] and peer['persistent_streams'] and peer['independent_local_stores']==3 and peer['peer_nodes_share_test_process']
  assert peer['head']==native['head'] and peer['height']==native['height'] and not peer['public_network']
 summary={**native,'cases':33,'reference_revision':build['reference_revision'],'build_sha256':sha(ROOT/'results/rust-build.json'),'corpus_sha256':sha(corpus),'evidence_sha256':sha(evidence),'checker_sha256':binary,'checker_artifact':str((out/'check-canonical').relative_to(ROOT)),'profile_sha256':m['profile_sha256'],'qualification_driver_sha256':sha(Path(__file__)),'peer':peer,'peer_checker_sha256':peer_binary,'peer_checker_artifact':str((out/'canonical-sync').relative_to(ROOT))if args.peer else None,'public_network':False,'global_header_admission':False,'history_verified_from_public_network':False,'scope':'Fixed early expansion-zero single-zone private profile only. Native admission, frontier score, atomic publication, immutable generation reads and crash/restart recovery. Optional raw-object acquisition uses three independent stores in one process over actual loopback Noise/Yamux; not public-network/full-protocol sync, independent OS-node or coordinator worker qualification.'}
 save(out/'summary.json',summary);save(ROOT/'results/latest-canonical.json',{'passed':True,'directory':str(out.relative_to(ROOT))})
 print(json.dumps({'passed':True,'directory':str(out.relative_to(ROOT)),'checks':native['checks'],'peer_checked':bool(peer),'global_full_consensus':False},indent=2))
if __name__=='__main__':main()
