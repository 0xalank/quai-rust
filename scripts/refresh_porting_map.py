#!/usr/bin/env python3
"""Refresh or check source accounting and native implementation scope."""
import argparse,collections,hashlib,json,re,subprocess
from pathlib import Path
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def owner(path):
    name=Path(path).name;stem=name.removesuffix('_test.go').removesuffix('.go')
    if path in ['VERSION','go.mod','go.sum']:return 'build_support'
    if path.startswith(('p2p/mocks/','p2p/protocol/mocks/')):return 'test_support'
    if path.startswith('common/stream_services'):return 'network_wire'
    if path.startswith('common/bootnodes'):return 'peer_discovery'
    if path.startswith('common/proto_common.'):return 'wire_schema'
    if path.startswith('core/types/proto_block.'):return 'wire_schema'
    if path.startswith(('core/types/auxpow','core/types/btc','core/types/bch','core/types/ltc','core/types/ravencoin','crypto/musig2/')):return 'auxpow'
    if path.startswith(('core/wquai_fees','core/types/fee_payment')):return 'fee_extensions'
    if path.startswith('core/types/'):
        if stem.startswith(('transaction','quai_tx','qi_tx','gen_access_tuple')) or stem in ['external_tx','utxo']:return 'transaction_primitives'
        if stem.startswith(('utxo_validate','stxo','etx_set','regular_qi_etx','qi_pubkey','remote_qi')):return 'qi_etx_state'
        if stem.startswith(('receipt','log','bloom9','gen_receipt','gen_log')):return 'receipts'
        if stem=='token_choice':return 'economics'
        if stem=='gaspool':return 'block_execution'
        return 'header_body'
    if path.startswith('consensus/misc/'):return 'economics'
    if path.startswith('consensus/'):return 'pow_consensus'
    if path.startswith('core/state/'):return 'account_state'
    if path.startswith('core/rawdb/'):return 'chain_storage'
    if path.startswith(('trie/','crypto/multiset/')):return 'trie'
    if path.startswith(('core/bloombits/','core/bloom_indexer','core/chain_indexer')):return 'indexes'
    if path.startswith('core/vm/'):return 'evm'
    if path.startswith(('core/genesis','core/gen_genesis','params/')):return 'chain_spec'
    if path.startswith(('core/tx_','core/txpool')):return 'mempool'
    if path.startswith(('core/worker','core/miner','stratum/')):return 'mining'
    if path.startswith('core/events'):return 'events'
    if path.startswith('core/exchange_controller'):return 'economics'
    if path.startswith(('core/block_validator','core/headerchain_validation')):return 'pow_consensus'
    if path.startswith(('core/poem','core/headerchain','core/slice','core/core','core/blocks')):return 'hierarchy'
    if path.startswith('core/bodydb'):return 'chain_storage'
    if path.startswith('core/'):return 'block_execution'
    if path.startswith('p2p/node/peerManager/'):return 'peer_discovery'
    if path.startswith('p2p/node/pubsubManager/'):return 'gossip'
    if path.startswith(('p2p/node/streamManager/','p2p/node/requestManager/','p2p/protocol/','p2p/pb/')):return 'network_wire'
    if path.startswith(('p2p/node/api.','p2p/node/p2p_services','quai/handler','quai/p2p_backend')):return 'sync'
    if path.startswith('p2p/'):return 'network_transport'
    if path.startswith('ethdb/'):return 'chain_storage'
    if path.startswith('rpc/'):return 'rpc_framework'
    if path.startswith(('internal/quaiapi/','quai/filters/','quai/api')):return 'rpc_methods'
    if path.startswith(('quaiclient/','quai/abi/')) or path=='interfaces.go':return 'client_abi'
    if path.startswith(('node/','cmd/','quai/quaiconfig/','quai/backend','quai/interface')):return 'node_runtime'
    if path.startswith('quai/bloombits'):return 'indexes'
    if path.startswith(('log/','metrics_config/','quaistats/','internal/telemetry/')):return 'telemetry'
    if path.startswith('event/'):return 'events'
    if path.startswith(('internal/build/','build/')):return 'build_support'
    if path.startswith('crypto/'):return 'crypto'
    if path.startswith(('common/','rlp/')):return 'utilities' if path.startswith('common/') else 'transaction_primitives'
    return 'unclassified'
NATIVE_MESSAGES={
 ('common/proto_common.proto','ProtoHash'),('common/proto_common.proto','ProtoAddress'),('common/proto_common.proto','ProtoLocation'),
 *[('core/types/proto_block.proto',n) for n in ['ProtoWorkObjectHeader','ProtoPowShareDiffAndCount','ProtoTransaction','ProtoAccessList','ProtoAccessTuple','ProtoTxIns','ProtoTxOuts','ProtoTxIn','ProtoOutPoint','ProtoTxOut','ProtoAuxPow','ProtoAuxTemplate','ProtoTermini']]
}
def generate(go,root):
    config=json.loads((root/'compatibility/porting-components.json').read_text());components=config['components'];ids={v['id'] for v in components}
    revision=subprocess.check_output(['git','rev-parse','HEAD'],cwd=go,text=True).strip()
    if revision!='33db82f2891b267ba898ca38bd2dd8bf75cbb072':raise SystemExit('Review a separate source baseline before changing the pinned reference')
    tracked=subprocess.check_output(['git','ls-files','-z'],cwd=go,text=True).split('\0')
    selected=sorted(p for p in tracked if p.endswith(('.go','.proto','.s','.S','.c','.h','.cpp')) or p in ['params/genesis_alloc.json','params/forfeiture_addresses.json','params/founder_forfeiture_addresses.json','VERSION','go.mod','go.sum'])
    dirty=subprocess.check_output(['git','diff','HEAD','--',*selected],cwd=go)
    if dirty:raise SystemExit('Pinned tracked source is modified')
    files=[];messages=[];packages=collections.defaultdict(lambda:collections.Counter())
    for path in selected:
        body=(go/path).read_text(errors='replace');component=owner(path)
        if component not in ids:raise SystemExit('Unclassified source: '+path)
        is_go=path.endswith('.go');is_test=path.endswith('_test.go')
        package=re.search(r'^package\s+(\w+)',body,re.M)
        generated=bool(re.search(r'Code generated.*DO NOT EDIT',body[:2000],re.S))
        files.append({'path':path,'component':component,'role':'go_test' if is_test else 'go_non_test' if is_go else 'schema' if path.endswith('.proto') else 'support_asset','sha256':sha(go/path),'go_package':package.group(1) if is_go and package else None,'generated':generated,'native_status':next(c['status'] for c in components if c['id']==component),'full_file_semantic_parity':False})
        if is_go:packages[str(Path(path).parent)]['test_files' if is_test else 'non_test_files']+=1
        if path.endswith('.proto'):
            for match in re.finditer(r'^\s*message\s+(\w+)\s*\{',body,re.M):
                name=match.group(1);messages.append({'source':path,'name':name,'line':body[:match.start()].count('\n')+1,'status':'bounded_native' if (path,name) in NATIVE_MESSAGES else 'raw_codec_only','raw_codec_native':True,'scope':'Fixture-qualified bounded codec only; not complete protocol validity.' if (path,name) in NATIVE_MESSAGES else 'Raw field codec qualified by frozen schema vectors; semantic reconstruction/validity not established.'})
    # Validate all existing implementation/evidence links; proposed crate names are plans.
    for c in components:
        assert not c['whole_go_component_parity']
        for path in c['go_references']:
            if not (go/path).is_file():raise SystemExit('Missing Go reference '+path+' in '+c['id'])
        for path in c['rust_sources']+c['evidence']:
            if not (root/path).is_file():raise SystemExit('Missing native evidence '+path+' in '+c['id'])
        counts=collections.Counter(v['role'] for v in files if v['component']==c['id']);c['file_counts']=dict(counts)
    default_build=json.loads((root/'results/rust-build.json').read_text())
    for path,digest in default_build['rust_sources'].items():
        if sha(root/path)!=digest:raise SystemExit('Native source differs from last build: '+path)
    latest=json.loads((root/'results/latest-rust.json').read_text());accepted=json.loads((root/latest['directory']/'summary.json').read_text());assert accepted['passed']
    assert accepted['build_sha256']==sha(root/'results/rust-build.json')
    published=json.loads((root/'compatibility/upstream-release.json').read_text())
    release=published['revision']
    assert published['tag']=='v0.57.0'and release==revision
    assert subprocess.check_output(['git','rev-parse',release+'^{commit}'],cwd=go,text=True).strip()==release
    ancestor=subprocess.run(['git','merge-base','--is-ancestor',release,revision],cwd=go).returncode==0
    delta=[{'revision':v.split('\t',1)[0],'subject':v.split('\t',1)[1]} for v in subprocess.check_output(['git','log','--format=%H%x09%s',release+'..'+revision],cwd=go,text=True).splitlines()]
    latest_native=json.loads((root/'results/native-validation.json').read_text())
    totals=collections.Counter(v['role'] for v in files)
    return {'schema':1,'reference':{'revision':revision,'version_file':(go/'VERSION').read_text().strip(),'tracked_source_clean':True,'release_tag':published['tag'],'release_revision':release,'release_is_ancestor':ancestor,'commits_after_release':delta,'public_release_source':published['source'],'public_release_checked_utc':published['checked_utc'],'upstream_version':'v0.57.0','upstream_profile':'official-upstream','source_built_utc':default_build['built_utc'],'source_qualification_utc':latest_native.get('qualification_completed_utc',default_build['built_utc']),'merged_pull_requests':[2757,2753],'preserved_local_fee_branch':'backup/pre-polaris-fees-20261110','live_network_configuration_qualified':False},'coverage_meaning':'Source accounting plus audited native scope, not line coverage or percentage of functional completion. Non-test Go includes generated files, mocks and support helpers.','completion_percentage':None,'network_ready':False,'native_full_block_validation':False,'totals':{'go_files':totals['go_non_test']+totals['go_test'],'go_non_test_files':totals['go_non_test'],'go_test_files':totals['go_test'],'proto_files':totals['schema'],'declared_proto_messages':len(messages),'bounded_native_proto_messages':sum(m['status']=='bounded_native' for m in messages),'raw_native_proto_messages':len(messages),'generated_go_files':sum(v['generated'] and v['path'].endswith('.go') for v in files),'all_inventoried_files':len(files),'unclassified_files':0},'native_evidence':{'rust_build_sha256':sha(root/'results/rust-build.json'),'rust_source_hashes':default_build['rust_sources'],'acceptance_report':latest['directory']+'/summary.json','rust_tests':latest_native['rust_tests'],'bounded_protocol_checks':latest_native['protocol_checks'],'standalone_capabilities_sha256':sha(root/'compatibility/standalone.json'),'network_gates_sha256':sha(root/'compatibility/network-gates.json'),'execution_checks':latest_native.get('execution_checks',0),'foundation_checks':latest_native.get('foundation_checks',0),'native_libp2p_transport':True,'native_libp2p_default_feature':False,'library_stateful_execution':True,'stateful_checks':latest_native.get('stateful_checks',0),'zone_execution_checks':latest_native.get('zone_execution_checks',0),'local_admission_checks':latest_native.get('local_admission_checks',0),'native_local_zone_admission':True,'hierarchy_confirmation_checks':latest_native.get('hierarchy_confirmation_checks',0),'native_hierarchy_confirmation_components':True,'strong_uncle_checks':latest_native.get('strong_uncle_checks',0),'native_pre_fork_strong_uncles':True,'native_pre_fork_weak_share_entropy':True,'weak_share_checks':latest_native.get('weak_share_checks',0),'native_nonempty_prefork_candidate_scoring':True,'private_branch_checks':latest_native.get('private_branch_checks',0),'native_private_branch_recovery':True,'native_complete_local_state_images':True,'native_incremental_secure_trie':True,'native_transactional_state_database':True,'state_database_checks':latest_native.get('state_database_checks',0),'canonical_execution_uses_state_database':True,'private_indexed_ancestry':True,'private_indexed_admission_catalog':True,'private_lazy_qi_state':True,'private_lazy_world_state':bool(latest_native.get('lazy_accounts')and latest_native.get('lazy_storage')),'private_lazy_etx_state':bool(latest_native.get('lazy_etx')),'private_lazy_lockup_state':bool(latest_native.get('lazy_lockups')),'activated_local_component_blocks':latest_native.get('activation_valid_blocks',0),'activated_local_valid_work_rejections':latest_native.get('activation_valid_work_rejections',0),'experimental_genesis_prefix_blocks':latest_native.get('genesis_prefix_blocks',0),'experimental_genesis_replay_checks':latest_native.get('genesis_replay_checks',0),'experimental_forward_sync_blocks':latest_native.get('forward_sync_blocks',0),'experimental_forward_sync_windows':latest_native.get('forward_sync_windows',0),'experimental_genesis_to_activation_qualified':False,'default_private_runtime_profile':'private-hierarchy-v1','experimental_private_runtime_profile':'private-execution-v2','activated_coincident_component_blocks':latest_native.get('coincident_distinct_conditional_blocks',0),'activated_coincident_checks':latest_native.get('coincident_component_checks',0),'activated_coincident_valid_work_rejections':latest_native.get('coincident_valid_work_rejections',0),'activated_coincident_checkpoint_ancestry_verified':False,'private_runtime_activated_coincident':False,'activation_checkpoint_ancestry_verified':False,'private_runtime_activated_transactions':False,'bound_hierarchy_execution':latest_native.get('bound_hierarchy_execution',False),'private_catalog_total_history_cap':None,'persisted_native_hierarchy_deltas':True,'private_canonical_execution_uses_state_database':True,'persisted_transition_cases':latest_native.get('persisted_transition_cases',0),'persisted_transition_go_comparisons':latest_native.get('persisted_transition_go_comparisons',0),'native_reward_distribution_components':True,'frozen_rpc_methods':8,'rpc_cases':accepted['rpc_comparisons'],'sdk_cases':accepted['sdk_comparisons'],'state_execution':False,'state_execution_scope':latest_native.get('scope','Bounded signed Quai/Qi settlement, incoming/historical components, fee feedback/reward-depth payout projections and 16 early whole-block differentials with native work and complete local image restoration. Two private branches qualify complete durable image/receipt/history recovery. A separate early mixed-order profile qualifies prime/region/zone components, rollup confirmation and derived incoming coinbases with durable context witnesses; broader nonlocal/fork/mixed history and global admission remain open. A fixed early private profile now composes native admission, frontier selection, atomic canonical publication/recovery and raw-block peer acquisition; it does not qualify full global consensus/sync.'),'native_etx_queue_state':True,'native_qi_input_prechecks':True,'native_application_wire':True,'native_evm_opcode_execution':True,'native_vm_checks':latest_native.get('vm_checks',0),'native_evm_full_execution':False,'polaris_checks':latest_native.get('polaris_checks',0),'reference_profile':'official-upstream','coordinator_checks':latest_native.get('coordinator_checks',0),'native_coordinator_search_and_reapplication':True,'native_pending_header_generation':False,'eligibility_checks':latest_native.get('eligibility_checks',0),'native_global_eligibility_components':True,'frontier_checks':latest_native.get('frontier_checks',0),'native_candidate_scoring':True,'native_coordinator_projections':True,'full_coordinator_qualified':False,'genesis_checks':latest_native.get('genesis_checks',0),'native_canonical_genesis_construction':True,'native_candidate_branch_storage':True,'genesis_expansion_qualified':False,'candidate_placement_checks':latest_native.get('candidate_placement_checks',0),'native_candidate_append_components':True,'hierarchy_header_checks':latest_native.get('hierarchy_header_checks',0),'native_bounded_prime_region_header_policy':True,'zone_header_checks':latest_native.get('zone_header_checks',0),'native_bounded_zone_header_policy':True,'native_conversion_base_fee_arithmetic':True,'history_checks':latest_native.get('history_checks',0),'native_candidate_ancestry_checks':True,'canonical_history_verified':False,'auxiliary_checks':latest_native.get('auxiliary_checks',0),'native_signed_auxiliary_bindings':True,'consensus_checks':latest_native.get('consensus_checks',0),'native_pow_kernels':True,'native_poem_rule_projections':True,'full_header_validation':False,'consensus_fork_choice':False,'native_private_canonical_publication':True,'private_canonical_checks':latest_native.get('private_canonical_checks',0),'native_private_raw_block_acquisition':True,'private_canonical_peer_checked':latest_native.get('private_canonical_peer_checked',False),'optional_private_node_runner':True,'independent_process_sync_checked':latest_native.get('independent_process_sync_checked',False),'private_peer_persistent_identity':True,'private_import_cancellation':True,'native_p2p_sync':False},'status_meanings':config['status_meanings'],'components':components,'files':files,'proto_messages':messages,'packages':[{'directory':k,**dict(v)} for k,v in sorted(packages.items())]}
def render(data):
    rows=[]
    for c in data['components']:
        counts=c['file_counts'];go=counts.get('go_non_test',0);test=counts.get('go_test',0)
        rows.append('| ['+c['title']+'](../go-quai/'+c['go_references'][0]+') | '+c['status']+' | '+str(go)+' / '+str(test)+' | '+c['native_scope'].replace('|','/')+' |')
    t=data['totals'];ref=data['reference']
    return f'''# Go-to-Rust coverage map

Audited {ref['source_qualification_utc'][:10]} against go-quai {ref['revision']} (VERSION {ref['version_file']}).
This accounts for all {t['go_files']} tracked Go files: {t['go_non_test_files']}
non-test and {t['go_test_files']} test files, plus {t['proto_files']} Protobuf schemas.
Non-test includes generated files, mocks and support helpers. Every file has
one primary owner in the machine map; components can have shared references.
Dependency source outside this checkout is not inventoried. The pinned go.mod/
go.sum hashes identify that separate compatibility audit surface.

**This is not a completion percentage or line/function test-coverage report.**
File counts do not establish behavioral equivalence. No entire Go subsystem
is marked fully ported. The RPC executable continues to serve frozen imported reads. Libraries now add
bounded nested execution, signed Quai/Qi settlement, UTXO/MuHash commitments,
16 linked early private whole-block execution differentials with reward payouts
and complete local state images, durable scratch checkpoints and optional private libp2p transport. Full block consensus, complete
historical economic state and global canonical native sync remain open. The fixed early private profile now qualifies native canonical publication and loopback raw-block acquisition; see docs/history/NATIVE_CANONICAL_RESULTS.md.
[Stateful results](docs/history/NATIVE_STATEFUL_RESULTS.md) record the exact scope.

The published {ref['release_tag']} release matches the pinned official source
reference, verified against the release page on {ref['public_release_checked_utc']}. The five local
fee-feature commits are preserved separately and are inactive in this profile.
Polaris activates at Prime terminus block 2,444,574. A network target must
choose and hash the exact fork/configuration profile, rather than rely on the
VERSION label. [Official release]({ref['public_release_source']}).

| Primary owner | Native scope status | Go non-test / test files | Native scope actually implemented |
| --- | --- | ---: | --- |
'''+ '\n'.join(rows)+f'''

All {t['declared_proto_messages']} declared messages have raw native field codecs,
qualified by frozen schema vectors. The designated bounded work-header/transaction/auxiliary
slice covers {t['bounded_native_proto_messages']} declarations; newer body/receipt/trie semantics are
tracked by component evidence. None of those counts is a completion percentage.
Bounded auxiliary semantic codecs and signed bindings now exist. Complete
execution/consensus and workshare/economic policy remain open. Optional private
libp2p transport exists; its existence does not qualify full native sync.

The current evidence is {data['native_evidence']['rust_tests']} Rust tests and
{data['native_evidence']['bounded_protocol_checks']:,} bounded protocol comparisons, plus
{data['native_evidence']['rpc_cases']} frozen RPC and {data['native_evidence']['sdk_cases']} SDK cases.
There are also {data['native_evidence']['execution_checks']:,} native execution-helper/live-ETX-state
comparisons and {data['native_evidence']['native_vm_checks']:,} bounded VM/state-gas
comparisons. [VM results](docs/history/NATIVE_EVM_RESULTS.md) and [foundation results](docs/history/NATIVE_FOUNDATION_RESULTS.md)
distinguish their scopes. There are also {data['native_evidence']['consensus_checks']:,}
consensus kernel/rule comparisons; [consensus results](docs/history/NATIVE_CONSENSUS_RESULTS.md)
list the remaining full-admission gates. Signed auxiliary verification adds
{data['native_evidence']['auxiliary_checks']:,} comparisons; [auxiliary results](docs/history/NATIVE_AUXILIARY_RESULTS.md)
record those bounded domains. Candidate ancestry and inclusion add
{data['native_evidence']['history_checks']:,} comparisons; [history results](docs/history/NATIVE_HISTORY_RESULTS.md)
separate controlled-hash policy from native signed KAWPOW cases. Imported history
remains unverified. Bounded zone header/conversion policy adds
{data['native_evidence']['zone_header_checks']:,} comparisons; [header results](docs/history/NATIVE_HEADER_RESULTS.md)
separate parent policy, candidate proofs and economic replay. Prime/region header
policy adds {data['native_evidence']['hierarchy_header_checks']:,} comparisons;
[hierarchy results](docs/history/NATIVE_HIERARCHY_RESULTS.md) record the wrapping/bitmap rules,
actual native proof fixtures and remaining canonical/hierarchy-placement gates.
Bounded candidate append preparation adds {data['native_evidence']['candidate_placement_checks']:,}
comparisons; [placement results](docs/history/NATIVE_PLACEMENT_RESULTS.md) distinguish computed
order/routing, local parent manifest/interlink commitments and PCRC candidate
transitions from canonical branch admission. Full placement qualification,
complete expansion/genesis state initialization, economic replay and fork choice remain open.
Canonical genesis/geometry/difficulty and multi-block candidate branch storage add
{data['native_evidence']['genesis_checks']:,} comparisons; [genesis results](docs/history/NATIVE_GENESIS_RESULTS.md)
separate payload construction and in-memory branch isolation from allocation
replay, durable canonical state and fork choice. Expanded-genesis startup,
local PoEM scores and coordinator projections add {data['native_evidence']['frontier_checks']:,}
comparisons; [frontier results](docs/history/NATIVE_FRONTIER_RESULTS.md) document explicit
legacy location-name aliasing and the open global coordinator/fork-choice gates.
Bounded multi-context ancestry, constraint tracing and PCRC frontier gates add
{data['native_evidence']['eligibility_checks']:,} comparisons;
[eligibility results](docs/history/NATIVE_ELIGIBILITY_RESULTS.md) separate six native coincident
blocks from controlled policy projections. Coordinator runtime integration,
economic replay and canonical fork choice remain open.
Recent admission, exact pinned-Go ordering, bounded leader search/retries and
pending-frontier reapplication add {data['native_evidence']['coordinator_checks']:,}
comparisons; [coordinator results](docs/history/NATIVE_COORDINATOR_RESULTS.md) distinguish
controlled projections from native-proof cases and open generation/runtime gates.
Polaris fork-boundary evidence adds {data['native_evidence']['polaris_checks']:,}
comparisons; [upstream migration results](docs/history/NATIVE_UPSTREAM_RESULTS.md) distinguish
ported binding/identity/Qi-input checks from unimplemented economic and network
changes. Fee feedback/reward-depth arithmetic and 16 linked early whole-block
execution replays add {data['native_evidence']['zone_execution_checks']:,} checks;
[docs/history/NATIVE_CONSENSUS_RESULTS.md](docs/history/NATIVE_CONSENSUS_RESULTS.md) distinguishes controlled
work projections, real native proof kernels and trusted local state-image
restoration from complete hierarchy/admission and canonical durability/sync.
Two private execution branches add {data['native_evidence']['private_branch_checks']:,}
state/receipt/history recovery and branch-switch comparisons;
[durable results](docs/history/NATIVE_DURABLE_RESULTS.md) separate explicit private selection
from PoEM canonical publication and production storage. A distinct local-zone
profile adds {data['native_evidence']['local_admission_checks']:,} real-Go matched
header/body/manifest/PCRC and execution checks, including valid-work mutations.
[Local admission results](docs/history/NATIVE_ADMISSION_RESULTS.md) preserve its pre-transaction,
non-dominant boundary. A separate early mixed-order profile adds
{data['native_evidence']['hierarchy_confirmation_checks']:,} comparisons for native
prime/region/zone append, committed rollups, derived incoming coinbases and durable
context witnesses. [Confirmation results](docs/history/NATIVE_CONFIRMATION_RESULTS.md) retain
its original single-zone/no-workshare boundary. A further strong-uncle corpus adds
{data['native_evidence']['strong_uncle_checks']:,} comparisons for full-target
ProgPoW inclusion, integer entropy, payouts and confirmed incoming coinbases.
[Uncle results](docs/history/NATIVE_UNCLES_RESULTS.md) retain their original strong-only scope. The current weak-share increment adds
{data['native_evidence']['weak_share_checks']:,} checks: controlled binary-precision
arithmetic plus native strong/weak share, PoEM, reward and branch execution.
[Weak-share results](docs/history/NATIVE_WEAKSHARES_RESULTS.md) distinguish bounded score
components from full canonical fork choice and retain remaining fork/sync gates.
Conditional activated coverage adds {data['native_evidence']['activated_local_component_blocks']}
local blocks and {data['native_evidence']['activated_coincident_component_blocks']} distinct
coincident zone blocks. The coincident suite has
{data['native_evidence']['activated_coincident_checks']:,} checks and
{data['native_evidence']['activated_coincident_valid_work_rejections']} valid-work mutations.
[Activated hierarchy results](docs/history/NATIVE_COINCIDENT_RESULTS.md) separate supplied-checkpoint
execution and branch recovery from authenticated ancestry and canonical admission.
These blocks do not extend the 24-block genesis-linked early canonical corpus.

The experimental genesis-only runtime qualifies
{data['native_evidence']['experimental_genesis_prefix_blocks']:,} pre-transaction blocks with
{data['native_evidence']['experimental_genesis_replay_checks']:,} replay/reopen assertions and
{data['native_evidence']['experimental_forward_sync_blocks']:,} separately acquired blocks across
{data['native_evidence']['experimental_forward_sync_windows']} bounded forward windows.
[Genesis/sync results](docs/history/NATIVE_GENESIS_SYNC_RESULTS.md) preserve the separate runtime
fingerprint, exact-executable replay-evidence reuse, fresh process checks and unqualified
transaction-activation boundary. These figures do not promote the public network gates.

Repeated benchmark operations are not additional feature coverage.

See the [machine-readable map](compatibility/go-quai-porting-map.json) for
every tracked path/hash, declared schema message, native source/evidence link,
component gap and proposed crate. [Network gates](NETWORK_READINESS.md) define
the route to a node; [allocation work](docs/ALLOCATION_PLAN.md) separates memory
traffic improvements from retained RAM and network readiness.

Refresh or detect drift without contacting a network:

    python3 scripts/refresh_porting_map.py --go-quai ../go-quai --write
    python3 scripts/refresh_porting_map.py --go-quai ../go-quai --check
    python3 scripts/check_network_plan.py

The [incremental state database](docs/history/NATIVE_STATE_DB_RESULTS.md) qualifies secure trie mutations, durable fork versions and pinned reads against actual Go. The [native private canonical path](docs/history/NATIVE_CANONICAL_DB_RESULTS.md) now uses it for keyed execution state; bounded map materialization, ancestry/catalog limits, production indexes and global replay remain open.

The generator rejects unclassified paths, missing references, changed native
source versus build evidence and a different/dirty pinned Go baseline. Review
component assignments when adding files; generated accounting is not a
substitute for functional review. Port externally observable behavior using
Rust implementations/libraries; a literal translation of every Go helper or
database file format is not required.
'''
def main():
    p=argparse.ArgumentParser();p.add_argument('--go-quai',required=True);p.add_argument('--project',default=str(Path(__file__).resolve().parents[1]));m=p.add_mutually_exclusive_group(required=True);m.add_argument('--write',action='store_true');m.add_argument('--check',action='store_true');a=p.parse_args()
    root=Path(a.project).resolve();go=Path(a.go_quai).resolve();data=generate(go,root);snapshot=root/'compatibility/go-quai-porting-map.json'
    if a.write:
        snapshot.write_text(json.dumps(data,indent=2)+'\n');(root/'PORTING_MAP.md').write_text(render(data))
    else:
        stored=json.loads(snapshot.read_text())
        if stored!=data:raise SystemExit('Porting map drifted; review assignments and refresh the snapshot')
        if (root/'PORTING_MAP.md').read_text()!=render(data):raise SystemExit('Readable map drifted from its generated source')
    print(json.dumps({'passed':True,'mode':'write' if a.write else 'check','totals':data['totals'],'network_ready':False,'completion_percentage':None},indent=2))
if __name__=='__main__':main()
