#!/usr/bin/env python3
"""Record/replay a versioned controlled-state corpus against the Rust gateway."""
import argparse
import copy
import datetime
import hashlib
import json

from chain_cases import assert_reference, cases
from compat_rpc import compare, decode, exchange, oracle, select_scenario
from lab import BASELINE, ROOT, Server, save, sha

CORPUS = ROOT / 'compatibility/corpus/controlled-v1.json'
HARNESS = ['scripts/compat_rpc.py', 'scripts/chain_cases.py', 'scripts/verify_chain.py', 'scripts/test_compat_rpc.py']
ENVIRONMENTS = [('zone', True), ('zone', False), ('region', True)]


def check_build():
    metadata = json.loads((ROOT / 'results/chain-build.json').read_text())
    if metadata['oracle_source_sha256'] != sha(ROOT / 'reference/chain/main.go'):
        raise SystemExit('Oracle source changed: rebuild before running')
    if metadata['oracle_binary_sha256'] != sha(ROOT / 'bin/quai-chain-oracle'):
        raise SystemExit('Oracle executable changed: rebuild before running')
    rust = json.loads((ROOT / 'results/build.json').read_text())
    if rust['binaries']['quai-rpc'] != sha(ROOT / 'bin/quai-rpc'):
        raise SystemExit('Rust executable changed: rebuild before running')
    for name, digest in rust['rust_sources'].items():
        if sha(ROOT / name) != digest:
            raise SystemExit('Rust source changed: rebuild before running')
    return metadata


def record(metadata):
    profiles = []
    for context, processing in ENVIRONMENTS:
        with oracle(context, processing) as (url, _):
            for scenario in ['canonical', 'alternate']:
                profile = select_scenario(url, scenario)
                print(f'record {context}, processing={processing}, {scenario}', flush=True)
                records = []
                for case in cases(profile):
                    expected = exchange(url, case['request'])
                    assert_reference(case, expected, profile)
                    records.append(dict(case, response=expected))
                profiles.append({'profile': profile, 'cases': records})
    corpus = {
        'schema': 1, 'recorded_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'scope': metadata['scope'], 'oracle_source_sha256': metadata['oracle_source_sha256'],
        'reference_revision': metadata['go_quai_revision'],
        'reference_sources_sha256': hashlib.sha256(json.dumps(metadata['go_quai_source_hashes'], sort_keys=True).encode()).hexdigest(),
        'profiles': profiles,
        'harness_source_hashes': {name: sha(ROOT / name) for name in HARNESS},
    }
    save(CORPUS, corpus)
    return corpus


def mutation_controls(corpus):
    count = 0
    for group in corpus['profiles']:
        if group['profile']['context'] != 'zone' or not group['profile']['processing_state']:
            continue
        for case in group['cases']:
            if case['id'] not in ['balance', 'storage', 'revert', 'large-id']:
                continue
            changed = copy.deepcopy(case['response'])
            body = json.loads(changed['body'])
            if case['id'] == 'large-id':
                body['id'] += 1
            elif 'error' in body:
                body['error']['message'] += ' changed'
            else:
                body['result'] = '0x0'
            changed['body'] = json.dumps(body)
            try:
                compare(case['request'], case['response'], changed)
            except AssertionError:
                count += 1
            else:
                raise AssertionError('Comparator accepted a seeded defect: ' + case['id'])
    return count


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--record', action='store_true', help='Regenerate the controlled-v1 corpus intentionally')
    args = parser.parse_args()
    metadata = check_build()
    if args.record:
        corpus = record(metadata)
    else:
        if not CORPUS.exists():
            raise SystemExit('No corpus: run once with --record to capture controlled API responses')
        corpus = json.loads(CORPUS.read_text())
    reference_sources = hashlib.sha256(json.dumps(metadata['go_quai_source_hashes'], sort_keys=True).encode()).hexdigest()
    if (corpus['reference_revision'] != metadata['go_quai_revision']
            or corpus['oracle_source_sha256'] != metadata['oracle_source_sha256']
            or corpus['reference_sources_sha256'] != reference_sources):
        raise SystemExit('Reference changed: review changes and intentionally version/record the corpus')
    for name, digest in corpus['harness_source_hashes'].items():
        if sha(ROOT / name) != digest:
            raise SystemExit('Compatibility harness changed; review before recording a new corpus: ' + name)
    selected = json.loads((ROOT / 'configs/recommended.json').read_text())
    configs = [('baseline', BASELINE), ('selected', selected)]
    failures, deviations, records = [], [], []
    passed, reference_checks, stale_controls = 0, 0, 0
    methods = {}
    for context, processing in ENVIRONMENTS:
        with oracle(context, processing) as (url, _):
            groups = [g for g in corpus['profiles'] if g['profile']['context'] == context
                      and g['profile']['processing_state'] == processing]
            for name, config in configs:
                with Server(config=config, extra={'mode': 'gateway', 'upstream': url}) as rust:
                    canonical_balance = None
                    for group in groups:
                        profile = select_scenario(url, group['profile']['scenario'])
                        for field in ['head_hash', 'snapshots', 'chain_id', 'location', 'scenario', 'enabled_methods', 'chain_config', 'rpc_gas_cap']:
                            if profile[field] != group['profile'][field]:
                                raise AssertionError('Oracle profile changed: ' + field)
                        print(f'check {name}: {context}, processing={processing}, {profile["scenario"]}', flush=True)
                        for case in group['cases']:
                            identity = f'{context}/{processing}/{profile["scenario"]}/{name}/{case["id"]}'
                            entry = {'case': identity, 'method': case['method'], 'passed': False}
                            direct, actual = None, None
                            try:
                                direct = exchange(url, case['request'])
                                assert_reference(case, direct, profile)
                                compare(case['request'], case['response'], direct)
                                reference_checks += 1
                                actual = exchange(rust.url, case['request'])
                                differences = compare(case['request'], case['response'], actual, notification_status=True)
                                deviations.extend({'case': identity, 'difference': d} for d in differences)
                                if case['id'] == 'balance' and context == 'zone' and processing:
                                    if profile['scenario'] == 'canonical':
                                        canonical_balance = case['response']
                                    else:
                                        try:
                                            compare(case['request'], canonical_balance, actual)
                                        except AssertionError:
                                            stale_controls += 1
                                        else:
                                            raise AssertionError('Alternate state accepted stale canonical balance')
                                entry['passed'] = True
                                passed += 1
                                if case['method'] != '<batch>':
                                    methods.setdefault(case['method'], []).append(identity)
                            except Exception as error:
                                entry.update(error=str(error), expected=case['response'], direct=direct, actual=actual)
                                failures.append(entry)
                            records.append(entry)
                    for method in ['quai_sendRawTransaction', 'admin_nodeInfo', 'debug_traceBlock',
                                   'personal_unlockAccount', 'quai_getOutpointsByAddressAndRange']:
                        raw = json.dumps({'jsonrpc': '2.0', 'id': 'denied', 'method': method, 'params': []})
                        value = decode(exchange(rust.url, raw)['body'])
                        if value.get('error', {}).get('code') != -32601:
                            failures.append({'case': method, 'error': 'Read profile boundary failed'})
    controls = mutation_controls(corpus)
    result = {
        'verified_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'passed': not failures, 'scope': metadata['scope'],
        'corpus_sha256': sha(CORPUS), 'oracle_binary_sha256': metadata['oracle_binary_sha256'],
        'oracle_source_sha256': metadata['oracle_source_sha256'],
        'rust_binary_sha256': sha(ROOT / 'bin/quai-rpc'),
        'reference_revision': metadata['go_quai_revision'],
        'harness_source_hashes': corpus['harness_source_hashes'],
        'reference_replay_checks': reference_checks, 'gateway_checks_passed': passed,
        'gateway_checks_total': len(records), 'methods_exercised': len(methods),
        'tested_configurations': dict(configs), 'state_profiles': [{'context': g['profile']['context'], 'processing_state': g['profile']['processing_state'], 'scenario': g['profile']['scenario'], 'head_hash': g['profile']['head_hash']} for g in corpus['profiles']],
        'mutation_controls_detected': controls, 'stale_state_controls_detected': stale_controls,
        'denied_method_checks': len(ENVIRONMENTS) * len(configs) * 5,
        'known_transport_deviations': deviations, 'failures': failures,
        'native_read_methods_verified': 0, 'synchronized_chain_methods_verified': 0,
        'client_sdk_acceptance': 'pending', 'full_reorg_validation': 'pending',
        'records': records,
    }
    save(ROOT / 'results/chain-conformance.json', result)
    save(ROOT / 'compatibility/coverage.json', {
        'scope': metadata['scope'], 'passed': not failures,
        'report': '../results/chain-conformance.json', 'corpus': 'corpus/controlled-v1.json',
        'methods': {method: {'evidence': 'controlled-state proxy', 'passing_cases': ids,
                             'native_implementation': False, 'synchronized_chain': False}
                    for method, ids in sorted(methods.items())},
    })
    print(json.dumps({k: v for k, v in result.items() if k not in ['records', 'failures']}, indent=2))
    if failures:
        print(json.dumps(failures[:8], indent=2))
        raise SystemExit(f'{len(failures)} compatibility failures; see results/chain-conformance.json')


if __name__ == '__main__':
    main()
