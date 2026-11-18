#!/usr/bin/env python3
"""Reproduce a Rust-only build from a reduced source copy with Go denied."""
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from lab import ROOT,save,sha

with tempfile.TemporaryDirectory(prefix='quai-no-go-build-') as directory:
    temporary=Path(directory);source=temporary/'source';source.mkdir()
    shutil.copytree(ROOT/'crates',source/'crates')
    shutil.copytree(ROOT/'configs',source/'configs')
    shutil.copy2(ROOT/'THIRD_PARTY_LICENSES.txt',source/'THIRD_PARTY_LICENSES.txt')
    for name in ['Cargo.toml','Cargo.lock']:shutil.copy2(ROOT/name,source/name)
    (source/'scripts').mkdir();(source/'fixtures/protocol').mkdir(parents=True)
    for name in ['build.py','lab.py']:shutil.copy2(ROOT/'scripts'/name,source/'scripts'/name)
    for name in ['work-headers.json','transactions.json','foundation.json','serial-vm.json','consensus.json','auxiliary.json','history.json','header.json','hierarchy.json','placement.json','genesis.json','frontier.json','eligibility.json','coordinator.json','polaris.json','stateful.json','settlement.json','qi-settlement.json','early-blocks.json','incoming.json','historical.json','authority.json','payouts.json','zone-blocks.json','private-branches.json','admission.json','confirmation.json','uncles.json','weakshares.json','canonical.json','state-db.json','activation.json','coincident.json','forward-wire.json','etx-family.json','controllers.json','choice-window.json','cubic-discount.json','prime-economics.json','prime-forward.json']:shutil.copy2(ROOT/'fixtures/protocol'/name,source/'fixtures/protocol'/name)
    assert not list(source.rglob('*.go'))
    shims=temporary/'deny';shims.mkdir();marker=temporary/'go-invoked'
    for name in ['go','gofmt','protoc']:
        path=shims/name;path.write_text('#!/bin/sh\n: > '+str(marker)+'\nprintf "Go is forbidden in the standalone build\\n" >&2\nexit 97\n');path.chmod(0o755)
    env={**os.environ,'PATH':str(shims)+os.pathsep+os.environ['PATH'],'GO_QUAI_DIR':str(temporary/'missing-go-checkout'),'CARGO_NET_OFFLINE':'true'}
    if 'CARGO_TARGET_DIR' not in env:env['CARGO_TARGET_DIR']=str(ROOT/'target')
    log_path=ROOT/'results/rust-no-go-build.log'
    with log_path.open('w') as log:subprocess.run([sys.executable,'-B',str(source/'scripts/build.py')],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    assert not marker.exists(),'A build step attempted to invoke Go'
    metadata=json.loads((source/'results/rust-build.json').read_text());assert not metadata['go_runtime_dependency'];assert metadata['test_profile']=='release'
    assert set(metadata['binary_source_targets'])=={'quai-rpc','quai-protocol-check'}
    expected_sources={str(p.relative_to(ROOT)):sha(p)for p in (ROOT/'crates').rglob('*.rs')}
    assert metadata['source_assets']==json.loads((ROOT/'results/rust-build.json').read_text())['source_assets'],'Reduced build assets differ'
    assert metadata['rust_sources']==expected_sources,'Reduced build omitted or changed Rust source'
    tree=subprocess.check_output(['cargo','tree','--locked','--offline','--workspace','--edges','normal'],cwd=source,env=env,text=True)
    for name in ['reqwest v','secp256k1-sys v','librocksdb-sys v']:assert name not in tree,name
    check=json.loads(subprocess.check_output([str(source/'bin/quai-protocol-check'),str(source/'fixtures/protocol/work-headers.json')],cwd=source,env=env,text=True));assert check['passed']
    tx_check=json.loads(subprocess.check_output([str(source/'bin/quai-protocol-check'),str(source/'fixtures/protocol/transactions.json')],cwd=source,env=env,text=True));assert tx_check['passed']
    foundation_check=json.loads(subprocess.check_output([str(source/'bin/quai-protocol-check'),str(source/'fixtures/protocol/foundation.json')],cwd=source,env=env,text=True));assert foundation_check['passed']
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-consensus'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    consensus=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-consensus'),str(source/'fixtures/protocol/consensus.json')],cwd=source,env=env,text=True));assert consensus['passed']
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-auxiliary'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    auxiliary=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-auxiliary'),str(source/'fixtures/protocol/auxiliary.json')],cwd=source,env=env,text=True));assert auxiliary['passed'] and auxiliary['full_kawpow_cases']==2
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-history'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    history=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-history'),str(source/'fixtures/protocol/history.json')],cwd=source,env=env,text=True));assert history['passed'] and history['native']['accepted']==4
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-header'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    header=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-header'),str(source/'fixtures/protocol/header.json')],cwd=source,env=env,text=True));assert header['passed'] and header['native']['accepted_component_bundles']==5
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-hierarchy'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    hierarchy=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-hierarchy'),str(source/'fixtures/protocol/hierarchy.json')],cwd=source,env=env,text=True));assert hierarchy['passed'] and hierarchy['native']['accepted_component_bundles']==21
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-placement'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    placement=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-placement'),str(source/'fixtures/protocol/placement.json')],cwd=source,env=env,text=True));assert placement['passed'] and placement['checks']==5478 and placement['native']['cases']==56 and placement['native']['accepted']==13
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-genesis'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    genesis=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-genesis'),str(source/'fixtures/protocol/genesis.json')],cwd=source,env=env,text=True));assert genesis['passed'] and genesis['checks']==2526 and genesis['native']['cases']==120 and genesis['native']['accepted']==30
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-frontier'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    frontier=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-frontier'),str(source/'fixtures/protocol/frontier.json')],cwd=source,env=env,text=True));assert frontier['passed'] and frontier['checks']==9988 and frontier['native']['attempts']==240 and frontier['native']['accepted']==54
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-eligibility'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    eligibility=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-eligibility'),str(source/'fixtures/protocol/eligibility.json')],cwd=source,env=env,text=True));assert eligibility['passed'] and eligibility['checks']==14328 and eligibility['native']['context_component_bundles']==18 and eligibility['native']['eligible_frontiers']==7
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-coordinator'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    coordinator=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-coordinator'),str(source/'fixtures/protocol/coordinator.json')],cwd=source,env=env,text=True));assert coordinator['passed'] and coordinator['checks']==42731 and coordinator['native']['accepted_searches']==16 and coordinator['native']['reapplication_passes']==12
    with log_path.open('a') as log:subprocess.run(['cargo','build','--locked','--offline','--release','-p','quai-consensus','--example','check-polaris'],cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
    polaris=json.loads(subprocess.check_output([str(Path(env['CARGO_TARGET_DIR'])/'release/examples/check-polaris'),str(source/'fixtures/protocol/polaris.json')],cwd=source,env=env,text=True));assert polaris['passed'] and polaris['checks']==198 and polaris['reference_revision']=='33db82f2891b267ba898ca38bd2dd8bf75cbb072'
    assert not marker.exists(),'Polaris verification attempted a forbidden tool'
    assert not marker.exists(),'Coordinator verification attempted a forbidden tool'
    assert not marker.exists(),'Eligibility verification attempted a forbidden tool'
    assert not marker.exists(),'Frontier verification attempted a forbidden tool'
    assert not marker.exists(),'Genesis verification attempted a forbidden tool'
    assert not marker.exists(),'Placement verification attempted a forbidden tool'
    assert not marker.exists(),'Hierarchy verification attempted a forbidden tool'
    assert not marker.exists(),'Header verification attempted a forbidden tool'
    assert not marker.exists(),'History verification attempted a forbidden tool'
    assert not marker.exists(),'Auxiliary verification attempted a forbidden tool'
    assert not marker.exists(),'Consensus verification attempted a forbidden tool'
    assert set(p.name for p in (source/'bin').iterdir())=={'quai-rpc','quai-protocol-check'}
    build_log=log_path.read_text();assert 'test frozen_go_frames_roots_gas_logs_and_traces_match ... ok' in build_log
    assert 'test frozen_go_orders_append_commitments_and_pcrc_match ... ok' in build_log
    assert 'test importing_a_candidate_does_not_skip_header_policy ... ok' in build_log
    assert 'test frozen_genesis_geometry_and_difficulty_match_go ... ok' in build_log
    assert 'test legacy_aliasing_requires_an_explicit_consistent_view ... ok' in build_log
    assert 'test frozen_poem_coordinator_and_pending_projections_match_go ... ok' in build_log
    assert 'test frozen_coordinator_ancestry_constraints_and_pcrc_match_go ... ok' in build_log
    assert 'test eligibility::limits::coordinator_entry_capacity_is_global_across_contexts ... ok' in build_log
    assert 'test missing_middle_ancestry_is_unavailable ... ok' in build_log
    assert metadata['rust_sources']=={str(p.relative_to(ROOT)):sha(p)for p in (ROOT/'crates').rglob('*.rs')},'Rust source changed during reduced verification'
    assert metadata['rust_sources']==json.loads((ROOT/'results/rust-build.json').read_text())['rust_sources'],'Default build differs from reduced source'
    assert 'test frozen_coordinator_search_ordering_and_reapplication_match_go ... ok' in build_log
    assert 'test native_node_entropy_rejects_unbound_body_before_proof_work ... ok' in build_log
    assert 'test recent_capacity_is_global_but_existing_full_slices_can_evict ... ok' in build_log
    assert 'test frozen_upstream_polaris_bindings_identities_and_inclusion_gates_match ... ok' in build_log
    assert 'test duplicate_qi_inputs_are_rejected_before_utxo_lookup ... ok' in build_log
    for name in ['nested_calls_creation_precompiles_and_etx_match_actual_go','signed_quai_settlement_and_transaction_finalization_match_actual_go','qi_settlement_utxo_commitments_and_outbound_etxs_match_actual_go','linked_early_blocks_match_go_state_processor_and_reject_forged_counters']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['fee_feedback_and_workshare_payouts_match_actual_go_branch','whole_zone_replay_with_native_work_and_restored_state_matches_go','state_image::tests::exact_images_preserve_raw_lockups_queue_metadata_and_legacy_muhash_ghosts','verifier::work_memo_tests::exact_header_and_scope_are_bound_without_promoting_work_to_block_proof']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['durable_execution_branches_and_receipts_match_actual_go_and_recover','process_exit_recovers_the_complete_last_selected_image_and_historical_context','journal::tests::head_switch_rechecks_ancestors_after_prior_success_without_cross_call_cache']:
        assert 'test '+name+' ... ok' in build_log,name
    assert 'test journal::tests::drop_unlocks_an_inherited_descriptor_before_reopen ... ok' in build_log
    for name in ['native_local_headers_execution_and_recovery_match_actual_go_validation','verifier::work_memo_tests::full_header_proof_memo_binds_bytes_rejects_mutation_and_never_promotes_weak_work','private_profile::tests::admission_profile_is_distinct_frozen_and_has_nontrivial_difficulty']:
        assert 'test '+name+' ... ok' in build_log,name
    assert 'test native_hierarchy_rollups_incoming_execution_and_recovery_match_actual_go ... ok' in build_log
    assert 'test native_strong_uncles_entropy_rewards_and_confirmation_match_actual_go ... ok' in build_log
    assert 'test weak_share_binary_rounding_matches_go_cubic_and_precision_boundaries ... ok' in build_log
    assert 'test native_weak_shares_poem_rewards_confirmation_and_recovery_match_actual_go ... ok' in build_log
    for name in ['native_admission_fork_choice_canonical_hashes_roots_and_recovery_match_go','private_canonical::tests::canonical_crash_recovers_old_or_complete_new_generation','private_canonical::tests::canonical_publication_rechecks_staged_objects_before_receipt','private_canonical::tests::canonical_record_bound_rejects_oversize_input_before_staging','private_canonical::tests::indexed_catalog_recovery_and_membership_remain_bound_to_their_generation']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['repeated_path_copy_and_materialization_drop_safely','persisted_incremental_tries_large_state_and_long_history_match_actual_go','state_db::tests::process_exit_recovers_old_or_complete_new_root_and_indexes','evm::state::cached_commitment_tests::incremental_validation_preserves_aggregate_slot_limits_after_failed_roots','bulk_snapshot_construction_matches_recomputation_and_remains_mutable','evm::state::cached_commitment_tests::unchanged_snapshots_share_derived_commitments_and_writes_detach_them']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['persisted_quai_qi_and_incoming_transitions_keep_frozen_go_results','state_store::tests::keyed_execution_versions_preserve_code_slots_queue_lockups_and_legacy_qi_ghosts','state_store::tests::missing_code_and_storage_preimages_never_restore_as_zero_state','private_branch::tests::keyed_store_recovers_committed_unselected_genesis_without_reinitializing','private_branch::tests::legacy_image_directories_are_preserved_and_refused_by_keyed_open','leaf_scans_verify_hashed_keys_and_enforce_entry_and_byte_bounds']:
        assert 'test '+name+' ... ok'in build_log,name
    for name in ['history::indexed_tests::indexed_candidate_reader_has_bounded_cache_without_a_total_history_cap','private_canonical::tests::indexed_recovery_reads_one_tip_and_excludes_competing_fork_records','confirmation::delta::tests::native_hierarchy_deltas_roundtrip_and_reject_wrong_branch_and_malformed_bytes']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['state_db::tests::version_header_cache_is_bounded_and_pinned_but_fresh_reads_detect_corruption','private_branch::tests::bounded_hierarchy_overlay_rebases_without_changing_native_execution']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['admission::tests::indexed_receipts_exceed_old_history_cap_with_pinned_forks_and_recovery','admission::tests::stale_duplicate_foreign_parent_and_corrupt_payload_cannot_change_selected_head','admission::tests::pinned_generation_survives_writer_reopen_and_later_appends','admission::tests::process_exit_recovers_complete_receipt_index_and_selected_head_transaction']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['state_store::tests::sparse_qi_restoration_reads_touched_points_and_persists_legacy_ghosts','state_store::tests::malformed_sparse_qi_records_are_unavailable_and_never_missing_membership','qi_settlement::storage_tests::signed_qi_backend_failure_remains_unavailable_without_state_or_gas_mutation']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['state_db::tests::pinned_snapshot_owns_database_until_its_final_read','state_db::tests::pinned_state_survives_writer_close_and_reopen_with_one_writer','state_db::tests::inode_writer_lease_rejects_aliases_and_all_but_one_concurrent_reopen','state_db::tests::replacing_state_path_during_commit_cannot_publish_or_reuse_writer','state_store::tests::sparse_qi_pinned_membership_survives_writer_close_reopen_and_child_commit']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['state_store::lazy_tests::lazy_accounts_and_slots_roundtrip_touched_writes_and_pinned_reopen','state_store::lazy_tests::stored_account_growth_and_deletion_do_not_require_total_slot_materialization','state_store::lazy_tests::malformed_lazy_account_and_code_are_unavailable_instead_of_missing','state_store::lazy_tests::nested_call_unavailable_storage_aborts_outer_writes_instead_of_becoming_revert','state_store::lazy_tests::nested_creation_unavailable_account_aborts_outer_storage_and_nonce_writes','state_store::lazy_tests::signed_quai_lazy_backend_failure_preserves_state_gas_and_selected_head','state_store::lazy_tests::stored_account_membership_crosses_the_old_total_account_limit','private_canonical::tests::previous_canonical_policy_is_refused_without_touching_database_files']:
        assert 'test '+name+' ... ok' in build_log,name
    assert 'test state_store::lazy_tests::diagnostic_account_export_stops_before_exceeding_aggregate_slot_budget ... ok' in build_log
    for name in ['lazy_etx_and_lockup_roundtrip_reads_only_touched_records_and_keeps_pinned_parents','lazy_etx_queue_exceeds_the_old_total_queue_limit_without_exporting_it','unavailable_queue_lockup_and_metadata_records_never_turn_into_absence','nested_lockup_backend_failure_aborts_outer_storage_changes','incoming_queue_backend_failure_preserves_state_and_all_gas_counters','authority_metadata_failure_is_unavailable_instead_of_a_false_command_result']:
        assert 'test state_store::aux_tests::'+name+' ... ok' in build_log,name
    for name in ['prepared_hierarchy_cannot_execute_another_block_or_chain','sealed_activated_mixed_ledger_append_execution_and_persistence_match_go','local_append_and_execution_must_bind_the_same_profile','absent_activation_redemption_history_is_unavailable_and_atomic','supplied_activation_checkpoint_cannot_acquire_canonical_authority']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['activated_coincident_hierarchy_execution_forks_and_local_recovery_match_go','checker::tests::activated_hierarchy_requires_complete_bound_views_and_keeps_runtime_scope']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['genesis_execution_profile_retains_go_canonical_prefix_fork_and_recovery','genesis_execution_profile_cannot_import_a_supplied_activation_checkpoint','private_profile::tests::hierarchy_permits_cannot_cross_frozen_runtime_contracts_with_the_same_heads']:
        assert 'test '+name+' ... ok' in build_log,name
    for name in ['local_header_proof_handoff_binds_exact_bytes_and_trusted_quorum','header_proof_handoff_excludes_weak_work_and_preserves_both_memo_bounds']:
        assert 'test verifier::work_memo_tests::'+name+' ... ok' in build_log,name
    for name in ['full_header_proof_can_supply_scope_bound_inclusion_work_without_another_kernel','full_proof_handoff_can_supply_inclusion_but_never_reverses_weak_role']:
        assert 'test verifier::work_memo_tests::'+name+' ... ok' in build_log,name
    assert 'test activated_all_etx_hierarchy_execution_forks_and_local_recovery_match_go ... ok' in build_log
    assert 'official_prime_controller_arithmetic_matches_all_frozen_go_cases ... ok' in build_log
    assert 'feedback_rejects_undefined_math_and_schedule_shortcuts_preserve_history ... ok' in build_log
    assert 'official_token_choice_window_warmup_rollover_forks_and_atomic_recovery ... ok' in build_log
    assert 'token_choice_recovery_rejects_malformed_rows_and_does_not_select_them ... ok' in build_log
    assert 'actual_runtime_dispatch_checks_all_defined_etx_families_and_bound_views ... ok' in build_log
    assert 'all_etx_native_runtime_refuses_supplied_checkpoint_and_cross_profile_state ... ok' in build_log
    assert 'all_etx_native_runtime_retains_real_genesis_prefix_forks_and_recovery ... ok' in build_log
    for name in ['private_branch::tests::shared_decode_memo_reuses_verified_records_but_never_crosses_fork_membership', 'private_branch::tests::warm_decode_memo_cannot_hide_later_ancestor_payload_corruption', 'private_branch::tests::shared_decode_memo_binds_versions_and_keeps_encoded_payload_budget']:
        assert 'test '+name+' ... ok' in build_log,name
    assert 'test official_conversion_cubic_binary_rounding_matches_frozen_go ... ok' in build_log
    assert 'test composed_parent_conversion_and_controller_fields_match_unmodified_go_verifier ... ok' in build_log
    assert 'test parent_controller_scope_and_etx_resource_bounds_remain_explicit ... ok' in build_log
    assert 'test official_prime_forward_quotes_refunds_linked_windows_and_atomic_recovery_match_go ... ok' in build_log
    rust_tests=sum(map(int,re.findall(r'test result: ok\. (\d+) passed',build_log)))
    report={'reference_revision':'33db82f2891b267ba898ca38bd2dd8bf75cbb072','polaris_checks':polaris['checks'],'polaris_prime_block':polaris['polaris_prime_block'],'coordinator_checks':coordinator['checks'],'coordinator_policy_searches':coordinator['policy']['search_cases'],'coordinator_native_searches':coordinator['native']['search_cases'],'coordinator_native_reapplications':coordinator['native']['reapplication_passes'],'coordinator_corpus_sha256':sha(ROOT/'fixtures/protocol/coordinator.json'),'source_build_sha256':sha(ROOT/'results/rust-build.json'),'imported_pre_anchor_walk_test_executed':True,'rust_source_count':len(metadata['rust_sources']),'eligibility_checks':eligibility['checks'],'native_multi_context_bundles':eligibility['native']['context_component_bundles'],'native_eligible_frontiers':eligibility['native']['eligible_frontiers'],'eligibility_corpus_sha256':sha(ROOT/'fixtures/protocol/eligibility.json'),'frontier_checks':frontier['checks'],'native_score_attempts':frontier['native']['attempts'],'accepted_native_scores':frontier['native']['accepted'],'constructed_startup_profiles':frontier['native']['constructed_startup_profiles'],'frontier_corpus_sha256':sha(ROOT/'fixtures/protocol/frontier.json'),'genesis_checks':genesis['checks'],'native_branch_cases':genesis['native']['cases'],'accepted_branch_insertions':genesis['native']['accepted'],'genesis_corpus_sha256':sha(ROOT/'fixtures/protocol/genesis.json'),'placement_corpus_sha256':sha(ROOT/'fixtures/protocol/placement.json'),'placement_checks':placement['checks'],'native_placement_cases':placement['native']['cases'],'accepted_candidate_preparations':placement['native']['accepted'],'hierarchy_checks':hierarchy['checks'],'native_hierarchy_cases':hierarchy['native']['cases'],'native_hierarchy_component_bundles':hierarchy['native']['accepted_component_bundles'],'binary_source_targets_verified':True,'complete_rust_source_copy_verified':True,'header_checks_total':header['checks'],'native_zone_header_cases':header['native']['cases'],'native_zone_component_bundles':header['native']['accepted_component_bundles'],'history_checks':history['checks'],'native_inclusion_cases':history['native']['cases'],'native_inclusion_accepted':history['native']['accepted'],'auxiliary_checks':auxiliary['checks'],'signed_auxiliary_kawpow_cases':auxiliary['full_kawpow_cases'],'consensus_checks':consensus['pow']['checks']+consensus['rules']['checks'],'full_size_epoch_cache_cases':consensus['pow']['full_size_epoch_caches'],'rust_tests':rust_tests,'mpt_drop_stability_test_executed':True,'explicit_writer_unlock_regression_test_executed':True,'sealed_local_header_proof_handoff_tests_executed':True,'one_way_full_proof_inclusion_reuse_tests_executed':True,'all_defined_etx_family_components_executed':True,'economic_controller_tests_executed':True,'economic_token_choice_window_test_executed':True,'all_defined_etx_native_runtime_tests_executed':True,'shared_history_decode_memo_correctness_tests_executed':True,'conversion_cubic_go_comparisons':4096,'prime_economic_field_go_comparisons':1024,'prime_forward_go_steps':1024,'all_etx_profile_asset_sha256':sha(ROOT/'configs/private-execution-v3.json'),'economic_controller_go_comparisons':3200,'test_profile':metadata['test_profile'],'stateful_execution_tests_executed':True,'native_hierarchy_confirmation_test_executed':True,'native_strong_uncle_test_executed':True,'native_weak_share_tests_executed':True,'private_canonical_tests_executed':True,'incremental_state_tests_executed':True,'native_canonical_keyed_state_tests_executed':True,'persisted_transaction_tests_executed':True,'indexed_ancestry_tests_executed':True,'indexed_admission_catalog_tests_executed':True,'lazy_qi_state_tests_executed':True,'lazy_world_state_tests_executed':True,'lazy_auxiliary_state_tests_executed':True,'activated_local_append_execution_tests_executed':True,'activated_coincident_hierarchy_tests_executed':True,'genesis_execution_profile_guards_and_prefix_tests_executed':True,'pinned_state_database_lifetime_tests_executed':True,'hierarchy_profile_asset_sha256':sha(ROOT/'configs/private-hierarchy-v1.json'),'stateful_corpus_sha256':{name:sha(ROOT/'fixtures/protocol'/name)for name in ['stateful.json','settlement.json','qi-settlement.json','early-blocks.json','incoming.json','historical.json','authority.json','payouts.json','zone-blocks.json','private-branches.json','admission.json','confirmation.json','uncles.json','weakshares.json','canonical.json','state-db.json','activation.json','coincident.json','forward-wire.json','etx-family.json','controllers.json','choice-window.json']},'private_profile_asset_sha256':sha(ROOT/'configs/private-native-v1.json'),'vm_frame_tests_executed':True,'vm_corpus_sha256':sha(ROOT/'fixtures/protocol/serial-vm.json'),'passed':True,'go_commands_attempted':0,'protoc_commands_attempted':0,'go_sources_in_build_copy':0,'go_checkout_present':False,'go_runtime_dependency':False,'upstream_client_in_default_dependency_graph':False,'protocol_checks':check['checks']+tx_check['checks']+foundation_check['checks'],'binaries':list(metadata['binaries']),'log':'results/rust-no-go-build.log','scope':'Default build from reduced source, Go/protoc commands denied and checkout absent. Native snapshot/primitive artifacts, isolated VM/account library and consensus components; not an independent validating node.'}
    save(ROOT/'results/rust-no-go-dependency-tree.json',{'tree':tree.replace(str(source),'<standalone-source>')})
    save(ROOT/'results/rust-no-go-build.json',report)
    print(json.dumps(report,indent=2))
