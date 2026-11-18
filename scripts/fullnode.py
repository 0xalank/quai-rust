"""Isolated, pinned go-quai local hierarchy for integration and read benchmarks."""
import contextlib
import json
import os
import random
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time

from compat_rpc import exchange
from lab import ROOT


def call(url, method, params=None):
    raw = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params or []})
    response = exchange(url, raw)
    value = json.loads(response['body'])
    if response['status'] != 200 or 'error' in value:
        raise RuntimeError(f'{method}: {value}')
    return value['result']


def ports():
    for _ in range(100):
        sockets = []
        try:
            # macOS may repeatedly allocate ephemeral ports too near 65535
            # for the zone offset. Choose within an explicit safe range.
            base = random.SystemRandom().randrange(20000, 45000)
            for offset in (0, 1, 199):
                sock = socket.socket()
                sock.bind(('127.0.0.1', base + offset))
                sockets.append(sock)
            return base
        except PermissionError:
            raise
        except OSError:
            pass
        finally:
            for sock in sockets:
                sock.close()
    raise RuntimeError('Could not allocate hierarchy ports')


class FullNode:
    def __init__(self, go_quai=None, consensus="progpow", funded=False):
        if consensus not in ("progpow", "blake3"):
            raise ValueError("Unsupported local consensus")
        self.consensus = consensus
        self.funded = funded
        if funded and consensus != "blake3":
            raise ValueError("Funded fixture requires blake3")
        self.go_quai = Path(go_quai or os.environ.get('GO_QUAI_DIR', ROOT.parent / 'go-quai')).resolve()
        self.temporary = None
        self.process = None

    def __enter__(self):
        try:
            return self._enter()
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def _enter(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='quai-rust-local-node-')
        self.directory = Path(self.temporary.name)
        (self.directory / 'params').mkdir()
        for name in ['VERSION', 'params/genesis_alloc.json', 'params/forfeiture_addresses.json']:
            shutil.copy2(self.go_quai / name, self.directory / name)
        self.env = dict(os.environ)
        self.env.pop('QUAI_LOCAL_FIXTURE_ALLOC_HASH', None)
        self.fixture = None
        if self.funded:
            self.fixture = json.loads(subprocess.check_output([
                str(ROOT / 'bin/quai-local-mine'), '--write-fixture', str(self.directory / 'params/genesis_alloc.json')], text=True))
            self.env['QUAI_LOCAL_FIXTURE_ALLOC_HASH'] = self.fixture['allocation_blake3']
            self.fixture['transaction_activation_height'] = 0
        self.base = ports()
        self.url = f'http://127.0.0.1:{self.base + 199}'
        self.args = [
            str(ROOT / ('bin/go-quai-local-fixture' if self.funded else 'bin/go-quai')), 'start',
            '--global.config-dir=' + str(self.directory / 'config'),
            '--global.data-dir=' + str(self.directory / 'data'),
            '--node.environment=local', '--node.consensus-engine=' + self.consensus, '--node.ipaddr=127.0.0.1', '--node.port=0',
            '--node.solo=true', '--node.portmap=false', '--node.force-public=true',
            '--node.telemetry=false', '--node.cache=64', '--node.stratum-enabled=false',
            '--rpc.http=true', '--rpc.http-addr=127.0.0.1', f'--rpc.http-port={self.base}',
            '--rpc.http-api=quai,eth,net', '--rpc.ws=false', '--rpc.health=false',
            '--metrics.enabled=false', '--metrics.metrics-addr=127.0.0.1', '--metrics.metrics-port=0', '--global.log-level=error', '--node.log-level=error',
        ]
        try:
            self.start()
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def start(self):
        self.log = (self.directory / 'process.log').open('a')
        self.process = subprocess.Popen(self.args, cwd=self.directory, stdout=self.log,
                                        stderr=self.log, start_new_session=True, env=self.env)
        deadline = time.monotonic() + 50
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                break
            try:
                chain_id = call(self.url, 'quai_chainId')
                genesis = call(self.url, 'quai_getBlockByNumber', ['0x0', False])
                if genesis and genesis.get('hash'):
                    self.profile = {'chain_id': chain_id, 'genesis_hash': genesis['hash'],
                                    'location': call(self.url, 'quai_nodeLocation'),
                                    'rpc_modules': call(self.url, 'rpc_modules'),
                                    'peer_count': call(self.url, 'net_peerCount'),
                                    'environment': 'local', 'consensus': self.consensus, 'funded_fixture': self.fixture, 'solo': True,
                                    'telemetry': False, 'bootstrapping': False}
                    if self.profile['peer_count'] != '0x0':
                        raise RuntimeError('Isolated node unexpectedly has peers')
                    return
            except (OSError, ValueError, RuntimeError):
                time.sleep(.3)
        text = (self.directory / 'process.log').read_text()
        raise RuntimeError('Local node failed to become ready:\n' + text[-7000:])

    def stop(self):
        if self.process and self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGINT)
            try:
                self.process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=5)
        if hasattr(self, 'log'):
            self.log.close()

    def restart(self):
        self.stop()
        self.start()

    def __exit__(self, *_):
        self.stop()
        if self.temporary:
            self.temporary.cleanup()


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('--go-quai')
    parser.add_argument('--consensus', choices=['progpow','blake3'], default='progpow')
    parser.add_argument('--funded', action='store_true', help='explicit test allocations and transaction activation at height zero')
    args = parser.parse_args()
    with FullNode(args.go_quai, consensus=args.consensus, funded=args.funded) as node:
        print(json.dumps({'url': node.url, 'profile': node.profile}, indent=2), flush=True)
        print('Press Ctrl-C to stop the isolated node.', flush=True)
        try:
            while True:
                time.sleep(1)
        except KeyboardInterrupt:
            pass
