#!/usr/bin/env python3
"""Package the optional loopback-only private-profile Rust node separately."""
import json,os,shutil,subprocess
from pathlib import Path
from lab import ROOT,save,sha
b=json.loads((ROOT/'results/rust-build.json').read_text())
for group in ['rust_sources','source_assets']:
 for name,digest in b[group].items():assert sha(ROOT/name)==digest,'Rebuild first: '+name
assert b['cargo_lock_sha256']==sha(ROOT/'Cargo.lock')
pointer=json.loads((ROOT/'results/latest-private-processes.json').read_text());proof=ROOT/pointer['directory']/'summary.json';q=json.loads(proof.read_text())
assert q['lazy_etx']and q['lazy_lockups']and q['lazy_accounts']and q['lazy_storage']and q['lazy_qi']and q['indexed_catalog']and q['indexed_ancestry']and q['private_canonical_execution_uses_state_database']and not q['complete_state_images_written']
assert q['passed']and q['separate_os_processes']and q['build_sha256']==sha(ROOT/'results/rust-build.json')and not q['global_full_consensus']and not q['canonical_public_sync']
subprocess.run(['cargo','build','--release','--locked','--offline','-p','quai-network','--features','native-transport','--example','private-node'],cwd=ROOT,check=True)
binary=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target'))/'release/examples/private-node';assert sha(binary)==q['node_binary_sha256']
bundle=ROOT/'dist/private-node';bundle.mkdir(parents=True,exist_ok=True)
allowed={'quai-private-node','CAPABILITIES.json','private-hierarchy-v1.json','README.md','NOTICE.md','LICENSE','THIRD_PARTY_LICENSES.txt','SHA256SUMS'}
assert not(set(p.name for p in bundle.iterdir())-allowed),'Unexpected optional bundle entries'
shutil.copy2(binary,bundle/'quai-private-node');shutil.copy2(ROOT/'configs/private-hierarchy-v1.json',bundle/'private-hierarchy-v1.json')
for name in ['NOTICE.md','LICENSE']:shutil.copy2(ROOT/'crates/quai-primitives'/name,bundle/name)
shutil.copy2(ROOT/'THIRD_PARTY_LICENSES.txt',bundle/'THIRD_PARTY_LICENSES.txt')
with(bundle/'NOTICE.md').open('a')as notice:
 for crate in ['quai-execution','quai-network','quai-consensus']:notice.write('\n\n'+(ROOT/'crates'/crate/'NOTICE.md').read_text())
forward_pointer=json.loads((ROOT/'results/latest-forward-processes.json').read_text());forward_path=ROOT/forward_pointer['directory']/'summary.json';forward=json.loads(forward_path.read_text());genesis_pointer=json.loads((ROOT/'results/latest-genesis-replay.json').read_text());genesis_path=ROOT/genesis_pointer['directory']/'summary.json';genesis=json.loads(genesis_path.read_text())
assert forward['passed']and genesis['passed']and forward['build_sha256']==genesis['build_sha256']==sha(ROOT/'results/rust-build.json')and forward['large_forward_result']['imported_blocks']==8192 and genesis['blocks']==8192 and not genesis['genesis_to_activation_qualified']
runtime_pointer=json.loads((ROOT/'results/latest-etx-runtime-processes.json').read_text());runtime_path=ROOT/runtime_pointer['directory']/'summary.json';runtime=json.loads(runtime_path.read_text());assert runtime['passed'] and runtime['build_sha256']==sha(ROOT/'results/rust-build.json') and runtime['node_binary_sha256']==sha(binary) and runtime['assertions']==15
save(bundle/'CAPABILITIES.json',{'kind':'optional-private-profile-prototype','reference_revision':b['reference_revision'],'reference_version':'v0.57.0','source_build_sha256':sha(ROOT/'results/rust-build.json'),'cargo_lock_sha256':sha(ROOT/'Cargo.lock'),'features':['native-transport'],'profile':'private-hierarchy-v1','profile_sha256':sha(ROOT/'configs/private-hierarchy-v1.json'),'chain_id':1337,'location':[0,0],'expansion':0,'transport':'enrolled IPv4 loopback Noise/Yamux','local_control':'bounded newline-delimited JSON on stdin/stdout','persistent_peer_identity':True,'private_canonical_admission':True,'private_canonical_reorg_and_recovery':True,'private_raw_block_acquisition':True,'separate_os_processes_checked':True,'go_build_or_runtime_dependency':False,'global_full_consensus':False,'canonical_public_sync':False,'live_rpc':False,'private_canonical_execution_uses_state_database':True,'execution_state_loading':'lazy-accounts-storage-etx-lockups-qi','lazy_qi':True,'lazy_accounts':True,'lazy_storage':True,'lazy_etx':True,'lazy_lockups':True,'indexed_ancestry':True,'indexed_catalog':True,'catalog_loading':'indexed-native-receipts-and-frontiers','ancestry_loading':'branch-pinned-on-demand','storage_format':'private-canonical-5 / keyed-state-3 / execution-codec-2 / hierarchy-delta-1','public_peer_access':False,'evidence_sha256':sha(proof),'identity_keys_in_bundle':False,'experimental_profiles':{'private-execution-v3':{'embedded_profile_sha256':sha(ROOT/'configs/private-execution-v3.json'),'genesis_only':True,'checkpoint_import_allowed':False,'defined_incoming_types':list(range(7)),'qualified_genesis_prefix_blocks':16,'activated_canonical_history_qualified':False,'public_sync_qualified':False,'process_evidence_sha256':sha(runtime_path)},'private-execution-v2':{'embedded_profile_sha256':sha(ROOT/'configs/private-execution-v2.json'),'genesis_only':True,'checkpoint_import_allowed':False,'qualified_genesis_prefix_blocks':8192,'genesis_to_activation_qualified':False,'public_sync_qualified':False,'genesis_evidence_sha256':sha(genesis_path),'forward_process_evidence_sha256':sha(forward_path)}}})
(bundle/'README.md').write_text('''# Private native node runner

This optional Rust prototype runs the fixed early private-hierarchy profile (chain 1337, zone [0,0], expansion zero). It validates raw blocks locally, selects the native private frontier and stores canonical generations with keyed account/storage/ETX/UTXO state in the native transactional database. Its qualification covers early pre-transaction histories and incoming coinbases. Full global consensus, public sync and live RPC remain unqualified.

This storage format requires a fresh private datadir. Earlier private formats are preserved and refused; this runner does not migrate them automatically. Ancestors and persisted hierarchy output load on demand from the selected branch, with bounded caches. Admission receipts, native frontiers and generation membership now use an indexed transactional catalog. Quai accounts, code for requested accounts, contract slots and Qi membership load on demand through pinned state snapshots; native persistence visits changed records. ETX queue, economic metadata and raw lockup records also read on demand; persistence visits touched keys. Pinned readers remain valid across writer reopen. Bounded acquisition batches and per-block/profile limits remain.

Start with a fresh private datadir:

    ./quai-private-node --profile private-hierarchy-v1 --datadir /private/tmp/quai-private-node

The first stdout line contains its peer ID, loopback address and status. Send one JSON object per stdin line, with a local request ID and an operation:

    {"id":1,"op":"status"}
    {"id":2,"op":"enroll","peer_id":"PEER_ID_FROM_ANOTHER_NODE"}
    {"id":3,"op":"sync","peer_id":"SOURCE_PEER_ID","address":"/ip4/127.0.0.1/tcp/PORT","head":"0xBLOCK_HASH"}
    {"id":4,"op":"cancel"}
    {"id":5,"op":"stop"}

The source must enroll the receiver's peer ID before serving it. Sync requires a known source address and an announced head; this runner does not discover or follow production heads automatically. Import raw local bundles with {"id":6,"op":"import","path":"/absolute/path/bundles.json"}; the file is an array of objects containing hex `zone` bytes and a `contexts` array of {"context":0,"bytes":"0x..."} witnesses. Every import still passes native private admission. No score, checkpoint or state image is accepted from a peer.

One import/acquisition worker is allowed. During import, `status` reports a nonblocking durable-admission counter; the execution-head CAS may still be completing, so this counter is not a selected height. Cancellation and shutdown preserve complete block publications; a valid prefix remains durable. Peer keys persist with checksums and owner-only permissions. Control lines, files, acquisition and transport have explicit limits. `--now SECONDS` supports a frozen lab clock. This console is a local control interface, separate from JSON-RPC.

The default standalone RPC bundle is separate. Source qualification and remaining gates are documented in the project file docs/history/NATIVE_PROCESS_RESULTS.md. The included profile is immutable source documentation, not a replaceable runtime network configuration.
''')
with(bundle/'README.md').open('a')as f:f.write('''
An experimental genesis-only mode is available with `--profile private-execution-v2`. Use a separate fresh datadir. Its embedded immutable contract does not accept checkpoints or state imports. The declared qualification covers 8192 native pre-transaction blocks from genesis, writer recovery and separate-process forward acquisition. Genesis-to-transaction-activation, later forks and general public consensus remain unqualified. The default `private-hierarchy-v1` contract is unchanged.

The local `follow` operation takes the same peer/address/head fields as `sync`, plus the target `height` and an optional `window_blocks` from 1 to 128. It discovers a shared native ancestor and requests numbered raw blocks/context witnesses in bounded windows. Each window retains at most 128 blocks and 32 MiB of responses, with its own deadline. Native validation and fork choice decide every publication; a lower-score remote head is never installed by fiat. Cancellation or an invalid block preserves only the completed native prefix. Numbered block and typed absence messages match pinned Go. This is explicit-source private acquisition, not automatic production head following.
''')
with(bundle/'README.md').open('a') as f:f.write('\nThe separate experimental `--profile private-execution-v3` enables all seven defined incoming ETX kinds in native admission. Its immutable contract requires a fresh genesis-only datadir and refuses supplied checkpoints and prior-profile state. Conditional supplied-history checks exercise all seven kinds; the 16-block process qualification covers actual genesis-linked import, transport and SIGKILL recovery before transaction activation. Full activated canonical ancestry, controllers/later forks and public sync remain unqualified.\n')
files={p.name:sha(p)for p in sorted(bundle.iterdir())if p.name!='SHA256SUMS'};(bundle/'SHA256SUMS').write_text(''.join(f'{h}  {n}\n'for n,h in files.items()))
save(ROOT/'results/private-node-bundle.json',{'passed':True,'directory':'dist/private-node','files':files,'source_build_sha256':sha(ROOT/'results/rust-build.json'),'qualification':str(proof.relative_to(ROOT)),'node_binary_sha256':q['node_binary_sha256'],'go_runtime_dependency':False,'global_full_consensus':False,'canonical_public_sync':False})
print(json.dumps({'passed':True,'directory':'dist/private-node','files':len(files)+1,'global_full_consensus':False},indent=2))
