#!/usr/bin/env python3
"""Build the standalone Rust artifacts. No Go toolchain, checkout or node needed."""
import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
from lab import ROOT, save, sha

env=dict(os.environ)
target=Path(env.get('CARGO_TARGET_DIR',ROOT/'target')).resolve()
env['CARGO_TARGET_DIR']=str(target)
# Check source targets before using any cached release executable.
workspace=json.loads(subprocess.check_output(['cargo','metadata','--locked','--offline','--no-deps','--format-version','1'],cwd=ROOT,env=env,text=True))
binary_targets={t['name']:Path(t['src_path']) for package in workspace['packages'] for t in package['targets'] if t['kind']==['bin']}
assert set(binary_targets)=={'quai-rpc','quai-protocol-check'},'Standalone binary source target is missing'
for name,path in binary_targets.items():assert path.is_file() and path.is_relative_to(ROOT/'crates'),name

for command in [['cargo','fmt','--check'],['cargo','test','--release','--locked','--workspace'],['cargo','build','--release','--locked','--workspace']]:
    subprocess.run(command,cwd=ROOT,env=env,check=True)
(ROOT/'bin').mkdir(exist_ok=True)
# Move previous mixed-profile artifacts out of the shipping output, reversibly.
reference_metadata=ROOT/'results/build.json'
if reference_metadata.exists():
    previous=json.loads(reference_metadata.read_text()).get('binaries',{})
    legacy=[name for name in previous if (ROOT/'bin'/name).is_file() and (name!='quai-rpc' or sha(ROOT/'bin'/name)==previous[name])]
    if legacy:
        archive=ROOT/'reference-artifacts'/datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
        archive.mkdir(parents=True)
        for name in legacy:shutil.move(str(ROOT/'bin'/name),archive/name)

for name in ['quai-rpc','quai-protocol-check']:
    shutil.copy2(target/'release'/name,ROOT/'bin'/name)
metadata={'built_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'reference_revision':'33db82f2891b267ba898ca38bd2dd8bf75cbb072','reference_version':'v0.57.0','profile':'standalone-rust','cargo_features':[],'test_profile':'release','go_toolchain_required':False,'go_checkout_required':False,'go_runtime_dependency':False,'consensus_validation':False,'chain_view':'frozen-unverified-import','rustc':subprocess.check_output(['rustc','--version'],text=True).strip(),'cargo_lock_sha256':sha(ROOT/'Cargo.lock'),'binaries':{name:sha(ROOT/'bin'/name) for name in ['quai-rpc','quai-protocol-check']},'binary_source_targets':{name:str(path.relative_to(ROOT)) for name,path in binary_targets.items()},'rust_sources':{str(p.relative_to(ROOT)):sha(p) for p in sorted((ROOT/'crates').rglob('*.rs'))},'source_assets':{str(p.relative_to(ROOT)):sha(p)for p in [ROOT/'Cargo.toml',ROOT/'configs/private-native-v1.json',ROOT/'configs/private-admission-v1.json',ROOT/'configs/private-hierarchy-v1.json',ROOT/'configs/private-execution-v2.json',ROOT/'configs/private-execution-v3.json',ROOT/'THIRD_PARTY_LICENSES.txt',ROOT/'scripts/build.py',*sorted((ROOT/'crates').rglob('Cargo.toml')),*sorted((ROOT/'crates').rglob('NOTICE.md')),*sorted((ROOT/'crates').rglob('LICENSE')),*sorted((ROOT/'crates').rglob('assets/*'))]}}
save(ROOT/'results/rust-build.json',metadata)
print('Built standalone Rust artifacts; see results/rust-build.json')

bundle=ROOT/'dist/standalone'
bundle.mkdir(parents=True,exist_ok=True)
allowed={'quai-rpc','quai-protocol-check','CAPABILITIES.json','NOTICE.md','LICENSE','THIRD_PARTY_LICENSES.txt','SHA256SUMS'}
if set(p.name for p in bundle.iterdir())-allowed:raise SystemExit('Standalone bundle contains unexpected files; choose a clean output directory')
for name in ['quai-rpc','quai-protocol-check']:shutil.copy2(ROOT/'bin'/name,bundle/name)
for name in ['NOTICE.md','LICENSE']:shutil.copy2(ROOT/'crates/quai-primitives'/name,bundle/name)
shutil.copy2(ROOT/'THIRD_PARTY_LICENSES.txt',bundle/'THIRD_PARTY_LICENSES.txt')
with (bundle/'NOTICE.md').open('a') as notice:
    for crate in ['quai-execution','quai-network','quai-consensus']:
        notice.write('\n\n'+(ROOT/'crates'/crate/'NOTICE.md').read_text())
save(bundle/'CAPABILITIES.json',{'reference_revision':'33db82f2891b267ba898ca38bd2dd8bf75cbb072','reference_version':'v0.57.0','polaris_prime_block':2444574,'library_polaris_binding_and_identity_rules':True,'polaris_full_economic_replay':False,'profile':'standalone-rust','go_toolchain_required':False,'go_runtime_dependency':False,'rpc_mode':'frozen snapshot','consensus_validation':False,'native_sync':False,'native_execution':False,'protocol_slice':'all pinned Protobuf field declarations; bounded canonical work headers, body headers, complete work objects, transaction/receipt/manifest/interlink/uncle commitments; transaction IDs/signature mathematics; typed-RLP encoding','native_transaction_state_validation':False,'library_execution_helpers':'bounded state journals, nested call/create, nine precompiles, signed Quai settlement, Qi spends/fees/UTXO MuHash commitments and outbound ETX emission; bounded private block execution with reward-depth payouts and incoming coinbases; incoming ETX dispatch, lockup calls, authority updates, indexed official allocations, historical redemption and UTXO trimming components; two private branches with complete local state/receipt/history recovery and explicit execution-head selection; separately pinned early three-context append, ETX rollup confirmation and derived incoming coinbase execution; early fixed private-profile native admission and PoEM-selected atomic canonical publication; global complete block admission remains unavailable','library_nested_execution':True,'library_signed_quai_settlement':True,'library_qi_settlement':True,'library_early_private_block_execution_replay':True,'library_incoming_etx_dispatch':True,'library_historical_economic_components':True,'library_private_execution_checkpoint_store':True,'library_private_complete_state_images':True,'library_private_branch_recovery':True,'library_local_zone_header_admission':True,'library_native_hierarchy_confirmation_components':True,'library_native_strong_uncle_components':True,'library_pre_fork_weak_share_entropy':True,'library_nonempty_prefork_candidate_scoring':True,'library_full_block_consensus_admission':False,'library_activated_local_append_execution':True,'library_activated_coincident_components':True,'private_runtime_activated_coincident':False,'library_bound_hierarchy_execution':True,'private_runtime_activated_transactions':False,'default_private_runtime_profile':'private-hierarchy-v1','experimental_private_execution_v2':True,'experimental_private_execution_v2_full_history_qualified':False,'library_private_canonical_admission_and_publication':True,'library_private_canonical_crash_recovery':True,'library_private_branch_pinned_indexed_ancestry':True,'library_persisted_native_hierarchy_deltas':True,'library_private_indexed_admission_catalog':True,'library_private_lazy_qi_state':True,'library_lazy_account_state':False,'library_lazy_etx_state':False,'library_private_canonical_catalog_full_history_replay':False,'optional_private_raw_block_acquisition':True,'optional_private_node_runner':True,'library_private_peer_persistent_identity':True,'library_private_import_cancellation':True,'native_vm_frame_library':True,'full_evm':False,'library_consensus_components':'native proof kernels, signed auxiliary quorum/coinbase/Merkle/Quai binding and KAWPOW block proof integration; bounded difficulty/entropy, share controllers and candidate zone ancestry/inclusion, bounded zone/prime/region header policy, canonical genesis construction, bounded initialization arithmetic, candidate branch storage/scoring and coordinator projections including bounded multi-context ancestry, constraint tracing and PCRC frontier gates with explicit legacy name aliasing, bounded recent admission/leader search/retries, native score-bound pending reapplication and virtual scheduling; candidate order/routing, parent manifest/interlink and PCRC preparation; prime controllers/ETX eligibility and reward/conversion/base-fee arithmetic; full consensus unavailable','library_hierarchy_header_components':True,'candidate_hierarchy_placement_qualified':False,'library_candidate_append_components':True,'library_genesis_construction':True,'library_candidate_scoring':True,'library_coordinator_projections':True,'library_global_eligibility_components':True,'library_coordinator_search_and_reapplication':True,'native_pending_header_generation':False,'full_coordinator_qualified':False,'library_candidate_branch_storage':True,'genesis_expansion_qualified':False,'library_zone_header_components':True,'library_conversion_base_fee_arithmetic':True,'canonical_history_verified':False,'library_signed_auxiliary_admission':True,'library_network_helpers':'application framing/topics/correlation; optional native-transport feature adds enrolled loopback-only Rust libp2p Noise/Yamux streams, identify, Kademlia and manually validated gossip; excluded from this default binary profile; bounded private raw-block acquisition and native canonical import; global full sync unavailable','typed_rlp_decode':False,'reference_feature_enabled':False})
checksums={p.name:sha(p) for p in sorted(bundle.iterdir()) if p.name!='SHA256SUMS'}
(bundle/'SHA256SUMS').write_text(''.join(f'{digest}  {name}\n' for name,digest in checksums.items()))
save(ROOT/'results/standalone-bundle.json',{'directory':'dist/standalone','files':checksums,'go_runtime_dependency':False})
print('Standalone bundle:',bundle)
