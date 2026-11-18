"""Scenario process lifetime and pinned SDK comparison helpers."""
import contextlib
import json
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile

from compat_rpc import get_json
from lab import ROOT


@contextlib.contextmanager
def scenario():
    with tempfile.TemporaryFile(mode='w+') as log:
        process = subprocess.Popen([str(ROOT / 'bin/quai-scenario-oracle')], cwd=ROOT,
                                   stdout=subprocess.PIPE, stderr=log, text=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                if not selector.select(30):
                    raise RuntimeError('Scenario startup timed out')
            line = process.stdout.readline()
            if not line:
                log.seek(0)
                raise RuntimeError(log.read())
            ready = json.loads(line)
            url = 'http://' + ready['address']
            yield url, get_json(url + '/profile'), process
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            process.stdout.close()


def sdk_compare(reference, candidate, profile, label, directory, subset='all'):
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    profile_file = directory / f'{label}-profile.json'
    profile_file.write_text(json.dumps(profile))
    reports = []
    for alias in ['quais57', 'quais58']:
        outputs = []
        for side, url in [('go', reference), ('rust', candidate)]:
            target = directory / f'{label}-{alias}-{side}.json'
            process = subprocess.run([
                'node', str(ROOT / 'sdk/acceptance.cjs'), alias, url,
                str(profile_file), str(target), subset,
            ], cwd=ROOT / 'sdk', capture_output=True, text=True, timeout=160)
            if process.returncode:
                raise RuntimeError(process.stderr + process.stdout)
            outputs.append(json.loads(target.read_text()))
        assert outputs[0]['packageVersion'] == outputs[1]['packageVersion']
        failures = []
        for left, right in zip(outputs[0]['results'], outputs[1]['results']):
            if left != right:
                failures.append({'case': left['name'], 'go': left, 'rust': right})
            if left['requiredSuccess'] and not left['ok']:
                failures.append({'case': left['name'], 'reference_failed_required_flow': left})
        assert len(outputs[0]['results']) == len(outputs[1]['results'])
        reports.append({'profile': label, 'package': alias,
                        'package_version': outputs[0]['packageVersion'],
                        'runtime_version_constant': outputs[0]['runtimeVersion'],
                        'cases': len(outputs[0]['results']), 'failures': failures})
        print(json.dumps({**reports[-1], 'failures': [v['case'] for v in failures]}), flush=True)
    return reports
