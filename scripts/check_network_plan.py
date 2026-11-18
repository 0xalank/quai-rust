#!/usr/bin/env python3
"""Check planning dependencies/evidence without opening sockets or running a node."""
import argparse
import json
from pathlib import Path


def check(root):
    profile = json.loads((root / 'compatibility/standalone.json').read_text())
    plan = json.loads((root / 'compatibility/network-gates.json').read_text())
    template = json.loads((root / 'compatibility/network-target.template.json').read_text())
    components = json.loads((root / 'compatibility/porting-components.json').read_text())
    gates = {gate['id']: gate for gate in plan['gates']}
    assert len(gates) == len(plan['gates']), 'Duplicate gate ID'
    done, visiting = set(), set()

    def visit(key):
        assert key in gates, 'Unknown gate: ' + key
        assert key not in visiting, 'Cyclic dependency: ' + key
        if key in done:
            return
        visiting.add(key)
        gate = gates[key]
        assert gate['status'] in {'open', 'partial', 'passed', 'deferred'}
        assert gate['acceptance'], 'Missing acceptance: ' + key
        for dependency in gate['depends_on']:
            visit(dependency)
        for proof in gate['proofs']:
            assert (root / proof).is_file(), 'Missing proof: ' + proof
            if proof.endswith('.json'):
                data = json.loads((root / proof).read_text())
                assert data.get('passed') is True, 'Proof did not pass: ' + proof
        if gate['status'] == 'passed':
            assert gate['proofs'], 'Passing gate without proof: ' + key
            assert all(gates[d]['status'] == 'passed' for d in gate['depends_on'])
        visiting.remove(key)
        done.add(key)

    for key in gates:
        visit(key)
    for component in components['components']:
        assert all(key in gates for key in component['milestones'])
        assert not component['whole_go_component_parity']
    assert not plan['runtime_go_allowed']
    assert plan['network_ready'] == all(
        gate['status'] == 'passed' for gate in gates.values()
        if gate['required_for_first_production_rpc_node']
    )
    assert profile['network_ready'] == plan['network_ready']
    assert template['kind'] == 'planning-template-not-runtime-config'
    assert template['qualified'] is False
    assert plan['target_qualified'] is False
    assert template['proposed_public_target']['confirmed'] is False
    # Unresolved fields make this a planning template, not a runnable configuration.
    required = ['selected_consensus_reference_revision', 'network', 'chain_id',
                'location', 'genesis_by_context', 'active_hierarchy',
                'pow_engines_by_fork', 'fork_manifest', 'fork_manifest_sha256',
                'resource_limits', 'fee_feature_policy', 'checkpoint',
                'private_bootnodes', 'public_bootnodes', 'pinned_network_dependencies',
                'captured_protocol_transcript']
    assert all(template[key] is None for key in required)
    assert not plan['network_ready'], 'Review this audit checker before promotion'
    assert not plan['network_access_performed_in_this_audit']
    assert len(profile['runtime_methods']) == 8
    assert profile['native_protocol']['transaction_codecs_and_ids'] != 'open'
    assert profile['native_protocol']['signature_verification'] != 'open'
    return {
        'passed': True,
        'gates': len(gates),
        'passed_gates': [key for key, gate in gates.items() if gate['status'] == 'passed'],
        'dependencies_acyclic': True,
        'proof_paths_present_and_passed': True,
        'component_milestones_valid': True,
        'target_qualified': False,
        'network_ready': False,
        'scope': 'Structural/evidence checks only; does not run client or establish network compatibility.'
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', default=str(Path(__file__).resolve().parents[1]))
    args = parser.parse_args()
    print(json.dumps(check(Path(args.project).resolve()), indent=2))


if __name__ == '__main__':
    main()
