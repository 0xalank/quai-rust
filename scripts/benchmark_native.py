#!/usr/bin/env python3
"""Paired read benchmarks: Go, forwarding gateway, hybrid and frozen Rust store."""
import argparse
import contextlib
import datetime
import json
import os
from pathlib import Path
import statistics
import subprocess
import threading
import time

from export_snapshot import atomic_write, capture, seal
from fullnode import FullNode, call
from lab import BASELINE, ROOT, Server, save, sha
from next_lab import scenario


class QuietServer(Server):
    def _monitor(self):
        pass  # One common sampler below measures every implementation.


def process_stats(pids):
    text = subprocess.check_output(['ps', '-o', 'pid=,rss=,time=', '-p', ','.join(map(str, pids))], text=True)
    result = {}
    for line in text.splitlines():
        pid, rss, cpu = line.split()
        parts = list(map(float, cpu.split(':')))
        seconds = sum(value * 60 ** i for i, value in enumerate(reversed(parts)))
        result[int(pid)] = {'rss_kib': int(rss), 'cpu_seconds': seconds}
    if set(result) != set(pids):
        raise RuntimeError('A measured server exited during the benchmark')
    return result


class Sampler:
    def __init__(self, pids):
        self.pids = pids
        self.stop = threading.Event()
        self.peak = 0
        self.error = None

    def __enter__(self):
        self.before = process_stats(self.pids)
        self.peak = sum(v['rss_kib'] for v in self.before.values())
        def sample():
            while not self.stop.wait(.1):
                try:
                    self.peak = max(self.peak, sum(v['rss_kib'] for v in process_stats(self.pids).values()))
                except Exception as error:
                    self.error = str(error)
                    return
        self.thread = threading.Thread(target=sample, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stop.set()
        self.thread.join(timeout=3)
        self.after = process_stats(self.pids)
        if self.error:
            raise RuntimeError(self.error)
        self.cpu = sum(v['cpu_seconds'] for v in self.after.values()) - sum(v['cpu_seconds'] for v in self.before.values())


def measure(source, source_pid, snapshot, mode, method, params, expected, seconds, directory):
    params_file, expected_file = directory / 'params.json', directory / 'expected.json'
    save(params_file, params)
    save(expected_file, expected)
    start = time.monotonic()
    with contextlib.ExitStack() as stack:
        if mode == 'go':
            url, pids = source, [source_pid]
        else:
            extra = {'mode': mode}
            if mode in ['gateway', 'hybrid']:
                extra['upstream'] = source
            if mode in ['snapshot', 'hybrid']:
                extra['snapshot'] = str(snapshot)
            server = stack.enter_context(QuietServer(config=BASELINE, extra=extra))
            url = server.url
            pids = [server.process.pid] + ([source_pid] if mode != 'snapshot' else [])
        startup = time.monotonic() - start
        with Sampler(pids) as resources:
            process = subprocess.run([
                str(ROOT / 'bin/quai-read-load'), '--url', url, '--method', method,
                '--params', str(params_file), '--expected', str(expected_file),
                '--seconds', str(seconds), '--concurrency', '8',
            ], capture_output=True, text=True, timeout=seconds + 30,
               env=dict(os.environ, GOMAXPROCS='8'))
            if process.returncode:
                raise RuntimeError(process.stdout + process.stderr)
            values = json.loads(process.stdout)
            assert values['failed_requests'] == 0
        values.update(mode=mode, params=params, expected_sha256=sha(expected_file), sampled_peak_server_rss_kib=resources.peak,
                      server_cpu_seconds=resources.cpu,
                      server_cpu_ms_per_call=resources.cpu * 1000 / values['calls'],
                      startup_seconds=startup,
                      server_process_count=len(pids))
        return values


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--rounds', type=int, default=3)
    parser.add_argument('--seconds', type=int, default=2)
    args = parser.parse_args()
    if not 2 <= args.rounds <= 10 or not 1 <= args.seconds <= 30:
        raise SystemExit('Use 2–10 rounds and 1–30 seconds per measurement')
    contract_names = ['scripts/benchmark_native.py','scripts/fullnode.py','scripts/export_snapshot.py','scripts/next_lab.py','reference/load/main.go','reference/scenarios/main.go']
    contract = {name: sha(ROOT / name) for name in contract_names}
    build = json.loads((ROOT / 'results/build.json').read_text())
    for name, digest in build['binaries'].items():
        assert sha(ROOT / 'bin' / name) == digest, 'Rebuild before benchmarking: ' + name
    run = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-native'
    out = ROOT / 'results' / run
    out.mkdir(parents=True, exist_ok=False)
    records = []
    modes = ['go', 'gateway', 'hybrid', 'snapshot']
    with contextlib.ExitStack() as stack:
        node = stack.enter_context(FullNode())
        scenario_url, profile, scenario_process = stack.enter_context(scenario())
        sources = [('fullnode-genesis', node.url, node.process.pid),
                   ('typed-scenarios', scenario_url, scenario_process.pid)]
        for label, source, source_pid in sources:
            view = capture(source)
            snapshot = out / (label + '.json')
            atomic_write(snapshot, seal(view))
            block_hash = view['canonical'][-1]
            workloads = [('block', 'quai_getBlockByHash', [block_hash, True])]
            if label == 'typed-scenarios':
                for receipt in view['receipts'][-3:]:
                    workloads.append(('receipt-' + receipt['type'], 'quai_getTransactionReceipt', [receipt['transactionHash']]))
            for name, method, params in workloads:
                expected = call(source, method, params)
                for number in range(args.rounds):
                    order = modes[number % len(modes):] + modes[:number % len(modes)]
                    for mode in order:
                        print(f'{label}/{name} round {number + 1}/{args.rounds}: {mode}', flush=True)
                        values = measure(source, source_pid, snapshot, mode, method, params,
                                         expected, args.seconds, out)
                        values.update(source=label, workload=name, round=number + 1,
                                      load_average=os.getloadavg())
                        records.append(values)
                        save(out / f'{label}-{name}-{number + 1}-{mode}.json', values)
    for name, digest in contract.items():
        assert sha(ROOT / name) == digest, 'Benchmark contract changed during run: ' + name
    medians = []
    for label, name in sorted({(r['source'], r['workload']) for r in records}):
        for mode in modes:
            values = [r for r in records if (r['source'], r['workload'], r['mode']) == (label, name, mode)]
            medians.append({'source': label, 'workload': name, 'mode': mode,
                            **{key: statistics.median(v[key] for v in values)
                               for key in ['calls_per_second', 'p95_ms', 'p99_ms', 'server_cpu_ms_per_call']},
                            'throughput_min': min(v['calls_per_second'] for v in values),
                            'throughput_max': max(v['calls_per_second'] for v in values),
                            'sampled_peak_server_rss_kib': max(v['sampled_peak_server_rss_kib'] for v in values)})
    result = {'run': run, 'rounds': args.rounds, 'seconds_per_measurement': args.seconds,
              'concurrency': 8, 'medians': medians, 'records': records,
              'validated_calls': sum(r['calls'] for r in records),
              'failed_requests': sum(r['failed_requests'] for r in records),
              'build': build, 'benchmark_source_sha256': sha(__file__),
              'contract_source_hashes': contract,
              'load_generator_source_sha256': sha(ROOT / 'reference/load/main.go'),
              'scope': 'Warm localhost reads on shared macOS. Real full-node genesis block; typed blocks/receipts use seeded API scenarios. Hybrid includes Go plus Rust resource use; frozen snapshot measures Rust alone. No block execution, database ingestion, mined-chain or production-SLA result.'}
    save(out / 'summary.json', result)
    save(ROOT / 'results/latest-native.json', {'directory': str(out.relative_to(ROOT))})
    print(json.dumps({'run': run, 'validated_calls': result['validated_calls'], 'failed_requests': result['failed_requests'], 'medians': medians}, indent=2))


if __name__ == '__main__':
    main()
