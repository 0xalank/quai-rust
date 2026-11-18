#!/usr/bin/env python3
"""Native replay concurrent with an explicit offline oracle; credit only a complete corpus.

Reads an existing gzip descriptor through the producer's final rename. Bounded
incremental decompression and pipe backpressure avoid retaining the history.
The producer is never started, stopped, changed or restarted by this driver.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import time
import zlib

from lab import ROOT, save, sha

p = argparse.ArgumentParser()
p.add_argument('--oracle', type=Path, required=True)
p.add_argument('--blocks', type=int, required=True)
p.add_argument('--scratch', type=Path, required=True, help='New private directory outside the repository; retained on failure.')
p.add_argument('--producer-pid', type=int, help='Already-running Python oracle generator, not its Go child.')
a = p.parse_args()
assert 1 <= a.blocks <= 300000
oracle = a.oracle.resolve()
scratch = a.scratch.resolve()
assert not scratch.exists() and not scratch.is_relative_to(ROOT)
build_path = ROOT / 'results/rust-build.json'
b = json.loads(build_path.read_text())
build = sha(build_path)
driver = sha(Path(__file__))
assert b['reference_revision'] == '33db82f2891b267ba898ca38bd2dd8bf75cbb072'
for group in ['rust_sources', 'source_assets']:
    for name, digest in b[group].items():
        assert sha(ROOT / name) == digest, 'Rebuild first: ' + name
complete_at_start = (oracle / 'source.json').exists()
def process_identity():
    assert a.producer_pid is not None
    result = subprocess.run(['ps', '-p', str(a.producer_pid), '-o', 'lstart=', '-o', 'stat=', '-o', 'args='],
                            capture_output=True, text=True)
    if result.returncode or not result.stdout.strip():
        return None
    fields = result.stdout.strip().split(None, 6)
    assert len(fields) == 7 and 'gen_genesis_stream.py ' in fields[6], 'Wrong producer process'
    if fields[5].startswith('Z'):
        return None
    # Scheduling state changes during legitimate CPU/IO progress. PID identity
    # is the stable start time and exact command, never the current R/S state.
    return ' '.join(fields[:5]) + ' ' + fields[6]
producer_identity = None
if not complete_at_start:
    assert a.producer_pid is not None, 'A live producer handle is required for a pending corpus'
    producer_identity = process_identity()
    assert producer_identity is not None, 'Oracle producer is not live'
def producer_live():
    if producer_identity is None:
        return False
    identity = process_identity()
    if identity is None:
        return False
    assert identity == producer_identity, 'Producer PID changed identity'
    return True

stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
out = ROOT / 'results' / ('follow-genesis-' + stamp)
out.mkdir()
shutil.copy2(Path(__file__), out / 'qualification-driver.py')
with (out / 'build.log').open('w') as log:
    subprocess.run(['cargo', 'build', '--release', '--locked', '--offline', '-p', 'quai-consensus',
                    '--example', 'check-genesis-stream'], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True)
binary = out / 'check-genesis-stream'
shutil.copy2(Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'release/examples/check-genesis-stream', binary)
binary_hash = sha(binary)
scratch.mkdir(mode=0o700)
(scratch / 'source').mkdir(mode=0o700)
deny = scratch / 'deny'; deny.mkdir()
marker = scratch / 'forbidden'
for name in ['go', 'gofmt', 'protoc']:
    path = deny / name
    path.write_text('#!/bin/sh\n: > ' + shlex.quote(str(marker)) + '\nexit 97\n')
    path.chmod(0o755)
env = {**os.environ, 'PATH': str(deny) + os.pathsep + os.environ['PATH'],
       'GO_QUAI_DIR': str(scratch / 'missing-go-checkout')}
source = oracle / 'blocks.jsonl.gz'
pending_source = oracle / 'blocks.jsonl.gz.pending'
while not source.exists() and not pending_source.exists():
    assert producer_live(), 'Producer stopped before creating its stream'
    time.sleep(1)
if not source.exists():
    source = pending_source
    if not source.exists():
        source = oracle / 'blocks.jsonl.gz'
mode = source.lstat().st_mode
assert stat.S_ISREG(mode) and not stat.S_ISLNK(mode)
opened_during_live_production = producer_live() if producer_identity is not None else False
save(out / 'operational-state.json', {'kind': 'active-run-not-acceptance', 'build_sha256': build,
     'blocks_target': a.blocks, 'producer_pid': a.producer_pid, 'producer_identity': producer_identity,
     'oracle': str(oracle), 'scratch': str(scratch), 'source_node_datadir': str(scratch / 'source'),
     'checker_sha256': binary_hash, 'driver_sha256': driver, 'passed': False,
     'full_consensus': False, 'canonical_public_sync': False, 'coverage_credited': 0})
compressed_hash = hashlib.sha256()
raw_hash = hashlib.sha256()
compressed_bytes = raw_bytes = 0
temporary_eof_waits = 0
max_decompressed_write_bytes = 0
decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
log_path = out / 'native-replay.log'
with source.open('rb') as stream, log_path.open('wb') as log:
    proc = subprocess.Popen([str(binary), str(scratch / 'source' / 'chain')], cwd=ROOT, env=env,
                            stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT)
    print(json.dumps({'native_checker_pid': proc.pid, 'producer_pid': a.producer_pid,
                      'target_blocks': a.blocks, 'directory': str(out.relative_to(ROOT))}), flush=True)
    try:
        while not decoder.eof:
            chunk = stream.read(65536)
            if not chunk:
                assert proc.poll() is None, 'Native checker exited before a complete gzip stream'
                assert producer_live(), 'Oracle stopped before its gzip trailer completed'
                temporary_eof_waits += 1
                time.sleep(1)
                continue
            compressed_hash.update(chunk)
            compressed_bytes += len(chunk)
            pending = chunk
            while pending:
                raw = decoder.decompress(pending, 1 << 20)
                pending = decoder.unconsumed_tail
                if raw:
                    raw_hash.update(raw)
                    raw_bytes += len(raw)
                    max_decompressed_write_bytes = max(max_decompressed_write_bytes, len(raw))
                    proc.stdin.write(raw)
                assert not decoder.unused_data, 'Unexpected concatenated/trailing gzip bytes'
        assert not stream.read(1), 'Trailing compressed data'
        proc.stdin.close()
        assert proc.wait(timeout=7200) == 0, log_path.read_text()[-3000:]
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
result = json.loads(log_path.read_text().splitlines()[-1])
assert result['kind'] == 'result' and result['passed'] and result['blocks'] == a.blocks
assert result['native_imports_this_run'] == a.blocks and result['resumed_from_height'] == 0
assert result['genesis_to_tip_local_receipts'] and not result['checkpoint_imported']
assert not result['full_consensus'] and not result['canonical_public_sync'] and not result['runtime_go_dependency']
while True:
    try:
        m = json.loads((oracle / 'source.json').read_text())
        break
    except (FileNotFoundError, json.JSONDecodeError):
        assert producer_live(), 'Oracle stopped without complete readable metadata'
        time.sleep(.1)
assert m['passed'] and m['blocks'] == a.blocks and m['validated_from_genesis']
assert not m['checkpoint_imported'] and not m['initial_allocations']
assert m['reference_revision'] == b['reference_revision']
assert m['compressed_stream_sha256'] == compressed_hash.hexdigest() == sha(oracle / 'blocks.jsonl.gz')
assert m['raw_stream_sha256'] == raw_hash.hexdigest()
assert m['head'] == result['head'] and m['signed_quai_transactions'] == result['signed_quai_transactions']
assert m['profile_sha256'] == sha(ROOT / 'configs/private-execution-v2.json')
assert m['driver_sha256'] == sha(ROOT / 'scripts/gen_genesis_stream.py')
for name, digest in m['oracle_sources'].items():
    assert sha(ROOT / name) == digest, name
for group in ['rust_sources', 'source_assets']:
    for name, digest in b[group].items():
        assert sha(ROOT / name) == digest, name
assert sha(build_path) == build and sha(Path(__file__)) == driver and sha(binary) == binary_hash and not marker.exists()
shutil.copy2(oracle / 'source.json', out / 'oracle-source.json')
report = {'passed': True, 'build_sha256': build, 'blocks': a.blocks, 'source_replay': result,
          'source_node_datadir': str(scratch / 'source'), 'source_native_datadir': str(scratch / 'source' / 'chain'),
          'stream_followed_while_oracle_active': opened_during_live_production,
          'oracle_completed_at_start': complete_at_start, 'producer_started_or_restarted': False,
          'temporary_eof_waits': temporary_eof_waits, 'max_decompressed_write_bytes': max_decompressed_write_bytes,
          'compressed_bytes': compressed_bytes, 'raw_bytes': raw_bytes,
          'compressed_stream_sha256': compressed_hash.hexdigest(), 'raw_stream_sha256': raw_hash.hexdigest(),
          'oracle_source_sha256': sha(oracle / 'source.json'), 'oracle_directory': str(oracle),
          'checker_sha256': binary_hash, 'checker_artifact': str(binary.relative_to(ROOT)), 'driver_sha256': driver,
          'go_commands_attempted': 0, 'checkpoint_imported': False, 'genesis_to_tip_local_receipts': True,
          'local_genesis_to_quai_activation_qualified': a.blocks > 259200 and result['signed_quai_transactions'] > 0,
          'full_consensus': False, 'canonical_public_sync': False, 'public_network': False,
          'scope': 'Native private source replay only. No coverage is granted until completed Go metadata, gzip CRC, '
                   'compressed/raw/source hashes, every native import, selected roots and counts all pass. '
                   'The retained native datadir was executed locally from genesis; no state image or checkpoint was supplied. '
                   'Full ETX/fork/expansion/coordinator/EVM/public sync qualification remains open.',
          'evidence_files': {path.name: sha(path) for path in out.iterdir() if path.is_file()}}
save(out / 'summary.json', report)
save(ROOT / 'results/latest-follow-genesis.json', {'passed': True, 'directory': str(out.relative_to(ROOT))})
print(json.dumps({key: report[key] for key in ['passed', 'blocks', 'stream_followed_while_oracle_active',
                                             'local_genesis_to_quai_activation_qualified']}, indent=2), flush=True)
