#!/usr/bin/env python3
"""Longer, post-selection fixture measurements; never change the tuning policy."""
import argparse
import datetime
import json
import os
import statistics

from lab import BASELINE, ROOT, Server, save, sha, verify_contract
from tune import measure


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--rounds', type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.rounds <= 10:
        raise SystemExit('Use 1–10 rounds')
    contract = verify_contract()
    build = json.loads((ROOT / 'results/build.json').read_text())
    for name, digest in build['binaries'].items():
        if sha(ROOT / 'bin' / name) != digest:
            raise SystemExit('Binary changed; rebuild before comparing')
    for name, digest in build['rust_sources'].items():
        if sha(ROOT / name) != digest:
            raise SystemExit('Source changed; rebuild before comparing')
    selected = json.loads((ROOT / 'configs/recommended.json').read_text())
    profiles = [
        {'name': 'payload-single', 'method': 'bench_payload',
         'requests': 65536, 'concurrency': 4, 'batch': 1},
        {'name': 'payload-batch32', 'method': 'bench_payload',
         'requests': 4096, 'concurrency': 4, 'batch': 32},
    ]
    run_id = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-compare'
    out = ROOT / 'results' / run_id
    out.mkdir(parents=True, exist_ok=False)
    save(out / 'environment.json', {
        'build': build, 'contract': contract, 'profiles': profiles,
        'comparison_script_sha256': sha(__file__),
        'selected_config': selected, 'load_average': os.getloadavg(),
        'scope': 'Post-selection synthetic payload checks, not new holdout promotion or a language-only comparison.',
    })
    names = ['rust-baseline', 'rust-selected', 'go-reference']
    rounds = []
    with Server('go') as oracle:
        for number in range(args.rounds):
            order = names[number % 3:] + names[:number % 3]
            results = {}
            for name in order:
                print(f'round {number + 1}/{args.rounds}: {name}', flush=True)
                config = selected if name == 'rust-selected' else BASELINE
                results[name] = measure(config, profiles, oracle.url,
                                        go=name == 'go-reference', seed=61971)
            record = {'round': number + 1, 'order': order, 'servers': results,
                      'load_average': os.getloadavg()}
            rounds.append(record)
            save(out / f'round-{number + 1:02d}.json', record)
    medians = {}
    for name in names:
        medians[name] = {
            'sampled_peak_rss_kib': max(r['servers'][name]['sampled_peak_rss_kib'] for r in rounds),
            'workloads': [{
                'name': profile['name'],
                'calls_per_second': statistics.median(r['servers'][name]['workloads'][i]['calls_per_second'] for r in rounds),
                'request_p95_ms': statistics.median(r['servers'][name]['workloads'][i]['request_p95_ms'] for r in rounds),
                'failed_requests': sum(r['servers'][name]['workloads'][i]['failed_requests'] for r in rounds),
            } for i, profile in enumerate(profiles)],
        }
    summary = {'run': run_id, 'rounds': rounds, 'medians': medians}
    save(out / 'summary.json', summary)
    save(ROOT / 'results/latest-compare.json', {'run_directory': str(out.relative_to(ROOT))})
    print(json.dumps({'run': run_id, 'medians': medians}, indent=2))


if __name__ == '__main__':
    main()
