#!/usr/bin/env python3
"""Previous qualified Rust datadir is refused byte-for-byte, without migration."""
import argparse,datetime,json,os,shutil,subprocess,tempfile
from pathlib import Path
from lab import ROOT,save,sha
from verify_private_processes import Node
DRIVER_DIGEST=sha(Path(__file__));HELPER_DIGEST=sha(ROOT/'scripts/verify_private_processes.py')
def main():
 parser=argparse.ArgumentParser();parser.add_argument('--baseline',type=Path,required=True);args=parser.parse_args();baseline=args.baseline.resolve()
 old_build=json.loads((baseline/'results/rust-build.json').read_text());old_proof=json.loads((baseline/'results/state-lease-final-verification.json').read_text());assert old_proof['passed']and old_proof['source_build_sha256']==sha(baseline/'results/rust-build.json')
 build=json.loads((ROOT/'results/rust-build.json').read_text());process=json.loads((ROOT/'results/latest-private-processes.json').read_text());process=json.loads((ROOT/process['directory']/'summary.json').read_text());assert process['passed']and process['lazy_accounts']and process['lazy_storage']and process['build_sha256']==sha(ROOT/'results/rust-build.json')
 old_manifest=json.loads((baseline/'results/private-node-bundle.json').read_text());new_manifest=json.loads((ROOT/'results/private-node-bundle.json').read_text());old_binary=baseline/old_manifest['directory']/'quai-private-node';new_binary=ROOT/new_manifest['directory']/'quai-private-node';assert sha(old_binary)==old_manifest['files']['quai-private-node']and sha(new_binary)==new_manifest['files']['quai-private-node']==process['node_binary_sha256']
 out=ROOT/'results'/('private-storage-upgrade-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'));out.mkdir();shutil.copy2(Path(__file__),out/'qualification-driver.py');shutil.copy2(ROOT/'scripts/verify_private_processes.py',out/'process-helper.py');shutil.copy2(old_binary,out/'previous-private-node');shutil.copy2(new_binary,out/'current-private-node');shutil.copy2(baseline/'results/rust-build.json',out/'previous-rust-build.json');shutil.copy2(ROOT/'results/rust-build.json',out/'current-rust-build.json')
 checks=[]
 def check(condition,label):assert condition,label;checks.append(label)
 def files(root):return{str(p.relative_to(root)):sha(p)for p in root.rglob('*')if p.is_file()}
 now=json.loads((ROOT/'fixtures/protocol/canonical.json').read_text())['now'];nodes=[]
 with tempfile.TemporaryDirectory(prefix='quai-format-upgrade-')as temporary:
  temporary=Path(temporary);directory=temporary/'old-datadir';shims=temporary/'deny';shims.mkdir();marker=temporary/'forbidden'
  for name in ['go','gofmt','protoc']:
   file=shims/name;file.write_text('#!/bin/sh\n: > '+str(marker)+'\nexit 97\n');file.chmod(0o755)
  env={**os.environ,'PATH':str(shims)+os.pathsep+os.environ['PATH'],'GO_QUAI_DIR':str(temporary/'missing-go')}
  try:
   old=Node(out/'previous-private-node',directory,now,env,out,'previous-initialize',nodes);status=old.status();check(status['height']==0,'previous qualified Rust binary creates a real genesis datadir');old.stop();old.cleanup();nodes.remove(old);before=files(directory);check(any(n.endswith('state.redb')for n in before),'previous datadir contains an execution database');check(any(n.endswith('catalog.redb')for n in before),'previous datadir contains an admission catalog')
   current=subprocess.run([str(out/'current-private-node'),'--profile','private-hierarchy-v1','--datadir',str(directory),'--now',str(now)],input='',capture_output=True,text=True,env=env,timeout=30);(out/'current-refusal-stdout.log').write_text(current.stdout);(out/'current-refusal-stderr.log').write_text(current.stderr)
   check(current.returncode!=0,'current binary refuses the previous canonical format');check('canonical directory profile'in current.stderr,'refusal identifies the old canonical policy');check('"event":"ready"'not in current.stdout,'new binary never reports ready on the old datadir');after=files(directory);check(before==after,'every old file name and byte hash is preserved after refusal')
   again=Node(out/'previous-private-node',directory,now,env,out,'previous-reopen',nodes);reopened=again.status();check(reopened['head']==status['head']and reopened['checkpoint']==status['checkpoint']and reopened['height']==status['height'],'previous binary can still reopen its exact selected generation');check(again.ready['peer_id']==old.ready['peer_id'],'previous peer identity remains intact');again.stop();again.cleanup();nodes.remove(again);check(not marker.exists(),'no Go or protoc command was attempted');save(out/'preserved-datadir-hashes.json',{'before':before,'after_refusal':after,'private_key_contents_included':False})
  finally:
   for node in nodes:node.cleanup()
 assert sha(Path(__file__))==DRIVER_DIGEST and sha(ROOT/'scripts/verify_private_processes.py')==HELPER_DIGEST
 check(sha(out/'previous-private-node')==sha(old_binary)and sha(out/'current-private-node')==sha(new_binary),'executed binaries remain immutable and match their qualified bundles')
 report={'passed':True,'build_sha256':sha(ROOT/'results/rust-build.json'),'previous_build_sha256':sha(baseline/'results/rust-build.json'),'reference_revision':build['reference_revision'],'checks':checks,'assertions':len(checks),'previous_storage_format':'private-canonical-4 / keyed-state-2 / execution-codec-1','current_storage_format':'private-canonical-5 / keyed-state-3 / execution-codec-2','previous_file_bytes_preserved':True,'automatic_migration':False,'previous_binary_reopen_checked':True,'runtime_go_dependency':False,'go_commands_attempted':0,'private_key_contents_in_artifacts':False,'public_network':False,'full_consensus':False,'canonical_public_sync':False,'driver_sha256':DRIVER_DIGEST,'process_helper_sha256':HELPER_DIGEST,'evidence_files':{p.name:sha(p)for p in out.iterdir()if p.is_file()},'scope':'Actual old and new qualified Rust private-node executables, isolated loopback profile and an ephemeral old genesis datadir. Checks explicit refusal without byte changes and successful old-binary reopen. This does not implement migration, import old data into the new format or enlarge native block/network consensus coverage.'};save(out/'summary.json',report);save(ROOT/'results/latest-private-storage-upgrade.json',{'passed':True,'directory':str(out.relative_to(ROOT))});print(json.dumps({'passed':True,'assertions':len(checks),'directory':str(out.relative_to(ROOT))},indent=2))
if __name__=='__main__':main()
