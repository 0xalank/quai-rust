#!/usr/bin/env python3
"""Serve a completed native streaming-replay datadir to an independent Rust receiver.

Works with the frozen pre-transaction pilot and the full activation stream.
No supplied checkpoint/state image is imported. No Go process is launched.
"""
import argparse
import datetime
import gzip
import hashlib
import json
import os
from pathlib import Path
import queue
import shlex
import shutil
import subprocess
import tempfile
import time

from lab import ROOT, save, sha
from verify_private_processes import Node

START_TRANSACTIONS = 259200
MAX_LINE_BYTES = 20 << 20
p = argparse.ArgumentParser()
p.add_argument('--source-proof', type=Path, required=True)
p.add_argument('--scratch', type=Path, help='New directory outside the repository; retained on failure for investigation.')
a = p.parse_args()
driver = sha(Path(__file__))
helper = sha(ROOT / 'scripts/verify_private_processes.py')
build_path = ROOT / 'results/rust-build.json'
build = json.loads(build_path.read_text())
build_hash = sha(build_path)
for group in ['rust_sources', 'source_assets']:
    for name, digest in build[group].items():
        assert sha(ROOT / name) == digest, 'Rebuild first: ' + name
source_proof_path = a.source_proof.resolve()
source_proof = json.loads(source_proof_path.read_text())
assert source_proof['passed'], 'source replay is not complete'
assert source_proof['build_sha256'] == build_hash, 'source replay build mismatch'
assert source_proof['genesis_to_tip_local_receipts'] and not source_proof['checkpoint_imported']
assert source_proof['driver_sha256'] == sha(ROOT / 'scripts/follow_genesis_oracle.py')
source_node_datadir = Path(source_proof['source_node_datadir']).resolve()
assert source_node_datadir.is_dir() and not source_node_datadir.is_relative_to(ROOT)
assert Path(source_proof['source_native_datadir']).resolve() == source_node_datadir / 'chain'
oracle = Path(source_proof['oracle_directory']).resolve()
m = json.loads((oracle / 'source.json').read_text())
assert m['passed'] and m['validated_from_genesis'] and not m['checkpoint_imported'] and not m['initial_allocations']
assert m['reference_revision'] == build['reference_revision'] == '33db82f2891b267ba898ca38bd2dd8bf75cbb072'
assert m['profile_sha256'] == sha(ROOT / 'configs/private-execution-v2.json')
assert m['driver_sha256'] == sha(ROOT / 'scripts/gen_genesis_stream.py')
for name, digest in m['oracle_sources'].items():
    assert sha(ROOT / name) == digest, name
corpus = oracle / 'blocks.jsonl.gz'
assert sha(corpus) == m['compressed_stream_sha256']
assert source_proof['oracle_source_sha256'] == sha(oracle / 'source.json')
assert source_proof['compressed_stream_sha256'] == m['compressed_stream_sha256']
assert source_proof['raw_stream_sha256'] == m['raw_stream_sha256']
replay = source_proof['source_replay']
assert replay['passed'] and replay['head'] == m['head'], 'source replay/Go head mismatch'
assert replay['blocks'] == m['blocks'] and source_proof['blocks'] == m['blocks']
assert replay['native_imports_this_run'] == m['blocks'] and replay['resumed_from_height'] == 0
assert replay['signed_quai_transactions'] == m['signed_quai_transactions']
assert replay['genesis_to_tip_local_receipts'] and not replay['checkpoint_imported']
for name, digest in source_proof['evidence_files'].items():
    artifact = source_proof_path.parent / name
    assert artifact.resolve().is_relative_to(source_proof_path.parent), name
    assert sha(artifact) == digest, 'source proof artifact mismatch: ' + name
assert sha(ROOT / source_proof['checker_artifact']) == source_proof['checker_sha256']
count = m['blocks']
assert 2 <= count <= 300000
boundary = START_TRANSACTIONS if count > START_TRANSACTIONS else count // 2
stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
out = ROOT / 'results' / ('followed-genesis-processes-' + stamp)
out.mkdir()
shutil.copy2(Path(__file__), out / 'qualification-driver.py')
shutil.copy2(ROOT / 'scripts/verify_private_processes.py', out / 'process-helper.py')
shutil.copy2(oracle / 'source.json', out / 'oracle-source.json')
shutil.copy2(source_proof_path, out / 'source-replay-proof.json')
bundle = json.loads((ROOT / 'results/private-node-bundle.json').read_text())
assert bundle['passed'] and bundle['source_build_sha256'] == build_hash
node_binary = ROOT / bundle['directory'] / 'quai-private-node'
assert sha(node_binary) == bundle['node_binary_sha256'] == bundle['files']['quai-private-node']
shutil.copy2(node_binary, out / 'private-node')
node_hash = sha(out / 'private-node')
checks = []
nodes = []
def message_field(raw, wanted):
    """Read one bounded field for a discovery hint; native admission validates it.

    These are pinned ProtoWorkObject.wo_header/parent_hash/value tags, used only
    to name a prefix already verified by the Rust source. The receiver also
    checks that this hash is the source's canonical hash at the target height.
    """
    assert len(raw) <= MAX_LINE_BYTES
    position = 0
    found = None
    def varint():
        nonlocal position
        value = 0
        for shift in range(0, 70, 7):
            assert position < len(raw)
            byte = raw[position]
            position += 1
            assert shift < 63 or byte <= 1
            value |= (byte & 127) << shift
            if byte < 128:
                return value
        raise AssertionError('overlong protobuf varint')
    while position < len(raw):
        tag = varint()
        field, wire = tag >> 3, tag & 7
        assert field > 0
        if wire == 0:
            varint()
        elif wire in {1, 5}:
            position += 8 if wire == 1 else 4
        else:
            assert wire == 2
            size = varint()
            end = position + size
            assert end <= len(raw)
            if field == wanted:
                assert found is None, 'duplicate discovery field'
                found = raw[position:end]
            position = end
        assert position <= len(raw)
    assert found is not None, 'missing discovery field'
    return found

def check(ok, message):
    assert ok, message
    checks.append(message)

owned_temporary = None
if a.scratch:
    scratch = a.scratch.resolve()
    assert not scratch.exists() and not scratch.is_relative_to(ROOT)
    scratch.mkdir(mode=0o700)
else:
    owned_temporary = tempfile.TemporaryDirectory(prefix='quai-genesis-native-processes-')
    scratch = Path(owned_temporary.name)
deny = scratch / 'deny'
deny.mkdir()
marker = scratch / 'forbidden'
for name in ['go', 'gofmt', 'protoc']:
    shim = deny / name
    shim.write_text('#!/bin/sh\n: > ' + shlex.quote(str(marker)) + '\nexit 97\n')
    shim.chmod(0o755)
env = {**os.environ, 'PATH': str(deny) + os.pathsep + os.environ['PATH'],
       'GO_QUAI_DIR': str(scratch / 'missing-go-checkout')}
def start(name, store):
    return Node(out / 'private-node', source_node_datadir if store == 'source' else scratch / store, m['now'], env, out, name, nodes,
                profile='private-execution-v2')
def roots(node, expected):
    s = node.status()
    check(s['height'] == expected['height'], 'native selected height')
    for key in ['evm_root', 'etx_root', 'utxo_root']:
        check(s[key] == expected[key], 'native ' + key)
    check(s['durable_checkpoint'] == s['checkpoint'], 'native selected state is durable')
    check(s['admitted_blocks'] == s['height'] + 1, 'complete local native receipt ancestry')
    check(not s['global_full_consensus'] and not s['canonical_public_sync'] and not s['runtime_go_dependency'],
          'runtime preserves private qualification scope')
    return s
def follow(receiver, source, target, height):
    job = receiver.send('follow', peer_id=source.ready['peer_id'], address=source.ready['address'],
                        head=target, height=height, window_blocks=128)
    end = time.monotonic() + max(14400, count * 2)
    progress = receiver.call('status')['admitted_blocks']
    changed = time.monotonic()
    while True:
        try:
            reply = receiver.wait(job, timeout=5)
            break
        except queue.Empty:
            check(receiver.p.poll() is None, 'receiver remains alive during forward sync')
            now = receiver.call('status')['admitted_blocks']
            if now > progress:
                progress = now
                changed = time.monotonic()
                print(json.dumps({'receiver_admitted_blocks': now, 'target_height': height}), flush=True)
            assert time.monotonic() < end and time.monotonic() - changed < 180, 'native forward progress stalled'
    check(reply['ok'], 'native forward worker completed')
    result = reply['result']
    check(result['target_admitted'] and result['head'] == target, 'native forward selects oracle head')
    check(result['peak_window_blocks'] <= 128 and result['peak_window_response_bytes'] <= 32 << 20,
          'forward acquisition obeys window limits')
    return result

try:
    expected = {}
    boundary_hash = None
    raw_hash = hashlib.sha256()
    with gzip.open(corpus, 'rb') as stream:
        while line := stream.readline(MAX_LINE_BYTES + 1):
            assert len(line) <= MAX_LINE_BYTES and line.endswith(b'\n'), 'bounded genesis stream record'
            raw_hash.update(line)
            row = json.loads(line)
            if row['kind'] == 'block' and row['height'] in {boundary, count}:
                expected[row['height']] = row
            if row['kind'] == 'block' and row['height'] == boundary + 1:
                parent = message_field(message_field(message_field(bytes.fromhex(row['work_object'][2:]), 1), 2), 1)
                assert len(parent) == 32
                boundary_hash = '0x' + parent.hex()
    assert raw_hash.hexdigest() == m['raw_stream_sha256'] and boundary_hash is not None
    check(True, 'all source blocks were already executed locally by the exact completed native replay')
    source = start('source-from-native-replay', 'source')
    roots(source, expected[count])
    check(source.status()['head'] == m['head'], 'node reopens locally executed checker data without state import')
    receiver = start('receiver-prefix', 'receiver')
    check(source.p.pid != receiver.p.pid, 'source and receiver run in independent OS processes')
    identity = receiver.ready['peer_id']
    source.call('enroll', peer_id=identity)
    first = follow(receiver, source, boundary_hash, boundary)
    check(first['imported_blocks'] == boundary, 'receiver independently validates every prefix block')
    roots(receiver, expected[boundary])
    receiver.stop()
    receiver = start('receiver-boundary-reopen', 'receiver')
    check(receiver.ready['peer_id'] == identity, 'boundary restart preserves peer identity')
    roots(receiver, expected[boundary])
    second = follow(receiver, source, m['head'], count)
    check(second['imported_blocks'] == count - boundary, 'receiver validates the suffix after its native restart')
    roots(receiver, expected[count])
    receiver.stop()
    receiver = start('receiver-final-reopen', 'receiver')
    roots(receiver, expected[count])
    known = receiver.call('follow', peer_id=source.ready['peer_id'], address='/ip4/127.0.0.1/tcp/1',
                          head=m['head'], height=count, window_blocks=128)
    check(known['requests'] == 0 and known['windows'] == 0, 'durable known head requires no network connection')
    check(not marker.exists(), 'native replay and processes never invoke Go or protoc')
finally:
    for node in nodes:
        node.cleanup()
    if owned_temporary:
        owned_temporary.cleanup()
assert driver == sha(Path(__file__)) and helper == sha(ROOT / 'scripts/verify_private_processes.py')
assert build_hash == sha(build_path) and node_hash == sha(out / 'private-node')
for group in ['rust_sources', 'source_assets']:
    for name, digest in build[group].items():
        assert sha(ROOT / name) == digest, name
save(out / 'checks.json', checks)
report = {'passed': True, 'build_sha256': build_hash, 'blocks': count, 'boundary_height': boundary,
          'signed_quai_transactions': m['signed_quai_transactions'], 'source_replay': replay,
          'prefix_forward': first, 'suffix_forward': second, 'head': m['head'], 'assertions': len(checks),
          'source_datadir_reopened_without_checkpoint_import': True, 'source_replay_executed_again': False,
          'source_replay_proof_sha256': sha(source_proof_path), 'source_node_datadir_reused': True,
          'node_binary_sha256': node_hash, 'separate_os_processes': True,
          'local_genesis_to_quai_activation_qualified': count > START_TRANSACTIONS and m['signed_quai_transactions'] > 0,
          'oracle_source_sha256': sha(oracle / 'source.json'), 'compressed_stream_sha256': sha(corpus),
          'driver_sha256': driver, 'process_helper_sha256': helper,
          'go_commands_attempted': 0, 'private_key_contents_in_artifacts': False,
          'full_consensus': False, 'canonical_public_sync': False, 'public_network': False,
          'scope': 'Completed native source replay is reused through its exact source-bound artifact; the source datadir was originally executed from genesis. Native datadir recovery into a serving Rust process, '
                   'and independent native receiver validation before and after a durable boundary restart. '
                   'An activated local signed-Quai suffix is credited only when present in the completed corpus. '
                   'Other ETX kinds, later forks, expansion, general coordinator, full EVM and public sync remain open.',
          'evidence_files': {path.name: sha(path) for path in out.iterdir() if path.is_file()}}
save(out / 'summary.json', report)
save(ROOT / 'results/latest-followed-genesis-processes.json', {'passed': True, 'directory': str(out.relative_to(ROOT))})
print(json.dumps({key: report[key] for key in ['passed', 'blocks', 'signed_quai_transactions', 'assertions',
                                             'local_genesis_to_quai_activation_qualified']}, indent=2), flush=True)
