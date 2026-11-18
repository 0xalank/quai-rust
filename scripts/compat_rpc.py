"""Strict RPC comparison and bounded local oracle lifetime management."""
import contextlib
from decimal import Decimal
import json
import selectors
import signal
import subprocess
import tempfile
import urllib.error
import urllib.request

from lab import ROOT


def decode(raw):
    return json.loads(raw, parse_float=Decimal)


def same(a, b):
    # Python normally treats True == 1; JSON compatibility must not.
    numeric = (int, Decimal)
    if type(a) in numeric and type(b) in numeric:
        return a == b
    if type(a) is not type(b):
        return False
    if isinstance(a, dict):
        return a.keys() == b.keys() and all(same(a[k], b[k]) for k in a)
    if isinstance(a, list):
        return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b))
    return a == b


def compare(request_raw, expected, actual, notification_status=False):
    """Only object order and outer batch order are ignored. Errors remain exact."""
    deviations = []
    if expected['status'] != actual['status']:
        request = decode(request_raw)
        items = request if isinstance(request, list) else [request]
        all_notifications = bool(items) and all(
            isinstance(v, dict) and v.get('jsonrpc') == '2.0'
            and isinstance(v.get('method'), str) and 'id' not in v for v in items)
        allowed = (notification_status and all_notifications
                   and not expected['body'].strip() and not actual['body'].strip()
                   and {expected['status'], actual['status']} == {200, 204})
        if allowed:
            deviations.append('documented empty-notification HTTP 200/204 difference')
        else:
            raise AssertionError(f'HTTP status: {expected["status"]} != {actual["status"]}')
    if not expected['body'].strip() or not actual['body'].strip():
        if bool(expected['body'].strip()) != bool(actual['body'].strip()):
            raise AssertionError('Unexpected empty/nonempty response')
        return deviations
    a, b = decode(expected['body']), decode(actual['body'])
    if isinstance(decode(request_raw), list) and isinstance(a, list) and isinstance(b, list):
        # Multiset matching preserves duplicate IDs and duplicate responses.
        unmatched = list(b)
        for value in a:
            match = next((i for i, other in enumerate(unmatched) if same(value, other)), None)
            if match is None:
                raise AssertionError('Missing or changed batch response')
            unmatched.pop(match)
        if unmatched:
            raise AssertionError('Additional batch response')
    elif not same(a, b):
        raise AssertionError('Response differs (IDs, fields, arrays, result or error)')
    return deviations


def exchange(url, raw):
    req = urllib.request.Request(url, data=raw.encode(), headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=15) as response:
            return {'status': response.status, 'body': response.read().decode()}
    except urllib.error.HTTPError as error:
        return {'status': error.code, 'body': error.read().decode()}


def get_json(url):
    with urllib.request.urlopen(url, timeout=15) as response:
        return json.load(response)


@contextlib.contextmanager
def oracle(node_context='zone', processing=True):
    args = [str(ROOT / 'bin/quai-chain-oracle'), '--context', node_context,
            '--processing-state=' + str(processing).lower()]
    with tempfile.TemporaryFile(mode='w+') as error_log:
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=error_log, text=True, cwd=ROOT)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                if not selector.select(30):
                    raise RuntimeError('Oracle startup timed out')
            line = process.stdout.readline()
            if not line:
                error_log.seek(0)
                raise RuntimeError(error_log.read())
            ready = json.loads(line)
            url = 'http://' + ready['address']
            yield url, get_json(url + '/profile')
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            process.stdout.close()


def select_scenario(url, name):
    return decode(exchange(url + '/scenario', json.dumps({'name': name}))['body'])
