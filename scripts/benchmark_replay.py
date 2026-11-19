#!/usr/bin/env python3
"""Rust against Go on the same mainnet blocks (ROADMAP.md, Phase 4).

Offline only: the Go side is the explicit oracle in reference/replay-bench,
built into the pinned go-quai module with `go build -overlay`, GOPROXY=off
and GOTOOLCHAIN=local, never into the Rust build. Nothing here touches the
network. Every run checks every proof-of-work header in full, on both sides.

    scripts/benchmark_replay.py build-go --go-quai ../go-quai --out BIN
    scripts/benchmark_replay.py pow --tool NAME=CMD ... --export DIR --from A --to B [--repeat 3] --out FILE
    scripts/benchmark_replay.py replay --rust BIN --go BIN --export DIR --to N --params DIR
        --jobs N --gomaxprocs N --workdir DIR [--repeat 3] --out FILE

Each run is measured from outside the process (macOS): CPU seconds from
wait4, peak RSS (ru_maxrss), peak physical footprint, bytes written to disk,
logical bytes written, instructions and cycles from proc_pid_rusage, taken
when the process has exited and before it is reaped. Load average and free
memory at the start and end of each run are recorded with it. A tool may
print marker lines ("<tool>: <name> start" / "<name> end") on stderr; the CPU
time of the process is sampled at each marker, so that a phase (a kernel
loop) can be measured without its setup. Runs of different tools alternate.
"""
import argparse
import ctypes
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PIN = '33db82f2891b267ba898ca38bd2dd8bf75cbb072'
FIELDS = ('user_time', 'system_time', 'pkg_idle_wkups', 'interrupt_wkups', 'pageins',
          'wired_size', 'resident_size', 'phys_footprint', 'proc_start_abstime',
          'proc_exit_abstime', 'child_user_time', 'child_system_time',
          'child_pkg_idle_wkups', 'child_interrupt_wkups', 'child_pageins',
          'child_elapsed_abstime', 'diskio_bytesread', 'diskio_byteswritten',
          'cpu_time_qos_default', 'cpu_time_qos_maintenance', 'cpu_time_qos_background',
          'cpu_time_qos_utility', 'cpu_time_qos_legacy', 'cpu_time_qos_user_initiated',
          'cpu_time_qos_user_interactive', 'billed_system_time', 'serviced_system_time',
          'logical_writes', 'lifetime_max_phys_footprint', 'instructions', 'cycles',
          'billed_energy', 'serviced_energy', 'interval_max_phys_footprint', 'runnable_time')


class RusageInfoV4(ctypes.Structure):
    _fields_ = [('uuid', ctypes.c_uint8 * 16)] + [(f, ctypes.c_uint64) for f in FIELDS]


class Timebase(ctypes.Structure):
    _fields_ = [('numer', ctypes.c_uint32), ('denom', ctypes.c_uint32)]


_libproc = ctypes.CDLL('/usr/lib/libproc.dylib')
_libc = ctypes.CDLL('/usr/lib/libSystem.B.dylib')
_tb = Timebase()
_libc.mach_timebase_info(ctypes.byref(_tb))
TICK_SECONDS = _tb.numer / _tb.denom / 1e9


def rusage_info(pid):
    """proc_pid_rusage(RUSAGE_INFO_V4) of a running or exited, unreaped process."""
    ri = RusageInfoV4()
    if _libproc.proc_pid_rusage(pid, 4, ctypes.byref(ri)) != 0:
        return None
    return {f: getattr(ri, f) for f in FIELDS}


class ThreadInfo(ctypes.Structure):
    _fields_ = [('user_time', ctypes.c_uint64), ('system_time', ctypes.c_uint64),
                ('cpu_usage', ctypes.c_int32), ('policy', ctypes.c_int32), ('run_state', ctypes.c_int32),
                ('flags', ctypes.c_int32), ('sleep_time', ctypes.c_int32), ('curpri', ctypes.c_int32),
                ('priority', ctypes.c_int32), ('maxpriority', ctypes.c_int32), ('name', ctypes.c_char * 64)]


def thread_cpu(pid):
    """CPU seconds of each live thread of pid, by name (proc_pidinfo)."""
    ids = (ctypes.c_uint64 * 4096)()
    n = _libproc.proc_pidinfo(pid, 6, 0, ids, ctypes.sizeof(ids))  # PROC_PIDLISTTHREADS
    out = {}
    for i in range(max(n, 0) // 8):
        info = ThreadInfo()
        if _libproc.proc_pidinfo(pid, 5, ctypes.c_uint64(ids[i]), ctypes.byref(info), ctypes.sizeof(info)) <= 0:
            continue
        name = info.name.decode(errors='replace') or f'thread-{i}'
        if i == 0:
            name = 'main'
        out[name] = out.get(name, 0) + (info.user_time + info.system_time) / 1e9
    return out


def cpu_of(ri):
    return (ri['user_time'] + ri['system_time']) * TICK_SECONDS


def free_memory():
    out = subprocess.run(['/usr/bin/vm_stat'], capture_output=True, text=True).stdout
    page = int(out.split('page size of ')[1].split()[0])
    pages = {}
    for line in out.splitlines()[1:]:
        if ':' in line:
            k, v = line.split(':', 1)
            try:
                pages[k.strip()] = int(v.strip().rstrip('.'))
            except ValueError:
                pass
    free = (pages.get('Pages free', 0) + pages.get('Pages speculative', 0)
            + pages.get('Pages purgeable', 0)
            + min(pages.get('Pages inactive', 0), pages.get('File-backed pages', 0)))
    return free * page


def du_bytes(path):
    if not Path(path).exists():
        return 0
    out = subprocess.run(['du', '-sk', str(path)], capture_output=True, text=True).stdout
    return int(out.split()[0]) * 1024 if out else 0


def measure(cmd, *, cwd=None, env=None, log=None):
    """Run cmd to completion and measure it from outside."""
    started = time.time()
    load0 = os.getloadavg()
    mem0 = free_memory()
    stdout = tempfile.TemporaryFile()
    proc = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=stdout, stderr=subprocess.PIPE)
    markers = {}
    lines = []

    def reader():
        for raw in proc.stderr:
            line = raw.decode(errors='replace').rstrip('\n')
            if line.endswith(' start') or line.endswith(' end'):
                ri = rusage_info(proc.pid)
                if ri is not None:
                    markers[line.split(': ', 1)[-1]] = {'cpu': cpu_of(ri), 'wall': time.time() - started}
            lines.append(line)
            if log is not None:
                log.write(line + '\n')
                log.flush()

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    threads = {}
    finished = threading.Event()

    def sampler():
        # Per-thread CPU while the process runs; the last sample is kept
        # (threads that exit earlier keep their last value). It never waits
        # for the process: the measurement below must find it unreaped.
        while not finished.is_set():
            try:
                for k, v in thread_cpu(proc.pid).items():
                    threads[k] = max(threads.get(k, 0), v)
            except OSError:
                pass
            time.sleep(1)

    ts = threading.Thread(target=sampler, daemon=True)
    ts.start()
    os.waitid(os.P_PID, proc.pid, os.WEXITED | os.WNOWAIT)
    finished.set()
    ri = rusage_info(proc.pid)
    _, status, ru = os.wait4(proc.pid, 0)
    wall = time.time() - started
    t.join(timeout=10)
    proc.returncode = os.waitstatus_to_exitcode(status)
    stdout.seek(0)
    out = stdout.read().decode(errors='replace')
    return {
        'cmd': cmd,
        'exit': proc.returncode,
        'wall_seconds': wall,
        'user_seconds': ru.ru_utime,
        'system_seconds': ru.ru_stime,
        'cpu_seconds': ru.ru_utime + ru.ru_stime,
        'max_rss_bytes': ru.ru_maxrss,
        'peak_footprint_bytes': ri and ri['lifetime_max_phys_footprint'],
        'disk_bytes_written': ri and ri['diskio_byteswritten'],
        'disk_bytes_read': ri and ri['diskio_bytesread'],
        'logical_bytes_written': ri and ri['logical_writes'],
        'instructions': ri and ri['instructions'],
        'cycles': ri and ri['cycles'],
        'markers': markers,
        'thread_cpu_seconds': dict(threads),
        'load_start': load0,
        'load_end': os.getloadavg(),
        'free_memory_start': mem0,
        'free_memory_end': free_memory(),
        'started_unix': started,
        'stdout': out,
        'stderr_tail': lines[-30:],
    }


def sha256_file(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def cmd_build_go(a):
    go = Path(a.go_quai).resolve()
    head = subprocess.check_output(['git', '-C', str(go), 'rev-parse', 'HEAD'], text=True).strip()
    if head != PIN:
        sys.exit(f'{go} is at {head}, not {PIN}')
    src = ROOT / 'reference/replay-bench'
    files = {
        go / 'cmd/quai-replay-bench/main.go': src / 'main.go',
        go / 'core/native_replay_bench.go': src / 'core-bench.go',
        go / 'consensus/progpow/native_replay_bench.go': src / 'progpow-bench.go',
    }
    out = Path(a.out).resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='quai-replay-bench-') as d:
        overlay = Path(d) / 'overlay.json'
        overlay.write_text(json.dumps({'Replace': {str(k): str(v) for k, v in files.items()}}))
        env = {**os.environ, 'GOPROXY': 'off', 'GOTOOLCHAIN': 'local'}
        subprocess.run(['go', 'build', '-overlay', str(overlay), '-ldflags',
                        '-X main.referenceRevision=' + PIN, '-o', str(out),
                        './cmd/quai-replay-bench'], cwd=go, env=env, check=True)
    info = {'binary': str(out), 'binary_sha256': sha256_file(out),
            'go': subprocess.check_output(['go', 'version'], text=True).strip(),
            'sources': {v.name: sha256_file(v) for v in files.values()}}
    print(json.dumps(info, indent=1))


def cmd_pow(a):
    tools = []
    for spec in a.tool:
        name, cmd = spec.split('=', 1)
        tools.append((name, shlex.split(cmd)))
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open('a') as f:
        for rep in range(a.repeat):
            for name, base in tools:
                if name.startswith('go'):
                    cmd = base + ['-export', str(Path(a.export).expanduser()), '-from', str(a.from_), '-to', str(a.to)]
                else:
                    cmd = base + [str(Path(a.export).expanduser()), str(a.from_), str(a.to)]
                cmd += a.extra or []
                r = measure(cmd)
                report = json.loads(r['stdout'].strip().splitlines()[-1]) if r['exit'] == 0 else None
                loop = None
                m = r['markers']
                if 'light loop start' in m and 'light loop end' in m and report:
                    cpu = m['light loop end']['cpu'] - m['light loop start']['cpu']
                    loop = {'cpu_seconds': cpu, 'cpu_ms_per_hash': 1e3 * cpu / report['inputs'],
                            'wall_seconds': m['light loop end']['wall'] - m['light loop start']['wall']}
                phases = {}
                for k in m:
                    if k.endswith(' start') and k[:-6] + ' end' in m and report:
                        cpu = m[k[:-6] + ' end']['cpu'] - m[k]['cpu']
                        phases[k[:-6]] = {'cpu_seconds': cpu, 'cpu_ms_per_input': 1e3 * cpu / report['inputs']}
                row = {'tool': name, 'repeat': rep, 'range': [a.from_, a.to], 'light_loop': loop, 'phases': phases,
                       'report': report, **{k: v for k, v in r.items() if k != 'stdout'}}
                f.write(json.dumps(row) + '\n')
                f.flush()
                print(name, rep, {k: round(v['cpu_ms_per_input'], 3) for k, v in phases.items()}, 'ms CPU/input',
                      report and report.get('sha256'), 'load', [round(x) for x in r['load_start']], flush=True)


def cmd_replay(a):
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    workdir = Path(a.workdir).resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    export = str(Path(a.export).expanduser().resolve())
    order = ['rust', 'go'] if not a.go_first else ['go', 'rust']
    if a.only:
        order = [a.only]
    with out.open('a') as f:
        for rep in range(a.repeat):
            for impl in order:
                datadir = workdir / f'{impl}-{rep}-{int(time.time())}'
                if datadir.exists():
                    sys.exit(f'{datadir} exists')
                env = dict(os.environ)
                if impl == 'rust':
                    cmd = [a.rust, '--datadir', str(datadir), '--log-level', 'info', 'replay',
                           '--export', export, '--state', '--to', str(a.to), '--jobs', str(a.jobs),
                           '--progress', '60'] + (['--pow-dag', a.pow_dag] if a.pow_dag else [])
                else:
                    env['GOMAXPROCS'] = str(a.gomaxprocs)
                    cmd = [a.go, 'replay', '-export', export, '-datadir', str(datadir),
                           '-params', str(Path(a.params).resolve()), '-to', str(a.to), '-progress', '60s']
                logp = out.parent / f'{out.stem}-{impl}-{rep}.log'
                with logp.open('w') as log:
                    r = measure(cmd, env=env, log=log)
                disk = du_bytes(datadir)
                summary = None
                if impl == 'rust':
                    p = datadir / 'replay/last.json'
                    summary = json.loads(p.read_text()) if p.exists() else None
                    heights = summary and summary.get('heights_this_run')
                    if heights:
                        heights -= 1  # genesis is counted
                    passed = r['exit'] == 0 and 'replay PASS' in r['stdout']
                else:
                    try:
                        summary = json.loads(r['stdout'].strip().splitlines()[-1])
                    except (ValueError, IndexError):
                        summary = None
                    heights = summary and summary.get('heights')
                    passed = r['exit'] == 0 and summary is not None and not summary.get('failure') \
                        and summary.get('last_zone') == a.to
                row = {'impl': impl, 'label': a.label if impl == 'rust' else '', 'repeat': rep, 'to': a.to,
                       'jobs': a.jobs, 'gomaxprocs': a.gomaxprocs,
                       'pow_dag': a.pow_dag, 'passed': passed, 'heights': heights, 'datadir_bytes': disk,
                       'summary': summary, **{k: v for k, v in r.items() if k != 'stdout'},
                       'stdout_tail': r['stdout'][-2000:]}
                if heights:
                    row['cpu_seconds_per_1000'] = 1000 * r['cpu_seconds'] / heights
                    row['blocks_per_second'] = heights / r['wall_seconds']
                    row['disk_bytes_written_per_height'] = (r['disk_bytes_written'] or 0) / heights
                    row['datadir_bytes_per_height'] = disk / heights
                f.write(json.dumps(row) + '\n')
                f.flush()
                print(impl, rep, 'PASS' if passed else 'FAIL', heights, 'heights',
                      round(r['cpu_seconds'], 1), 'cpu s', round(r['wall_seconds'], 1), 'wall s',
                      'rss', r['max_rss_bytes'] >> 20, 'MiB', 'written', (r['disk_bytes_written'] or 0) >> 20,
                      'MiB', 'load', [round(x) for x in r['load_start']], flush=True)
                if not a.keep:
                    shutil.rmtree(datadir, ignore_errors=True)
                if not passed:
                    sys.exit(f'{impl} run {rep} failed; log {logp}')


def spread(values):
    values = sorted(v for v in values if v is not None)
    if not values:
        return None
    mid = values[len(values) // 2] if len(values) % 2 else (values[len(values) // 2 - 1] + values[len(values) // 2]) / 2
    return {'median': mid, 'min': values[0], 'max': values[-1], 'n': len(values)}


def cmd_summarize(a):
    rows = [json.loads(line) for p in a.files for line in open(p)]
    key = 'impl' if rows and 'impl' in rows[0] else 'tool'
    groups = {}
    for r in rows:
        name = r[key] + (('/' + r['label']) if r.get('label') else '')
        groups.setdefault(name, []).append(r)
    metrics = ['cpu_seconds_per_1000', 'blocks_per_second', 'max_rss_bytes', 'peak_footprint_bytes',
               'disk_bytes_written_per_height', 'datadir_bytes_per_height']
    out = {}
    for name, rs in groups.items():
        if key == 'impl':
            out[name] = {m: spread([r.get(m) for r in rs]) for m in metrics}
            out[name]['instructions_per_height'] = spread([r['instructions'] / r['heights'] for r in rs if r.get('heights')])
            out[name]['passed'] = all(r['passed'] for r in rs)
            out[name]['load_start'] = [round(r['load_start'][0]) for r in rs]
        else:
            out[name] = {'cpu_ms_per_hash': spread([r['light_loop']['cpu_ms_per_hash'] for r in rs if r.get('light_loop')]),
                         'sha256': sorted({r['report']['sha256'] for r in rs if r.get('report')})}
    print(json.dumps(out, indent=1))


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest='cmd', required=True)
    b = sub.add_parser('build-go')
    b.add_argument('--go-quai', required=True)
    b.add_argument('--out', required=True)
    w = sub.add_parser('pow')
    w.add_argument('--tool', action='append', required=True, help='NAME=COMMAND; a NAME starting with go gets Go flags')
    w.add_argument('--export', required=True)
    w.add_argument('--from', dest='from_', type=int, required=True)
    w.add_argument('--to', type=int, required=True)
    w.add_argument('--repeat', type=int, default=3)
    w.add_argument('--out', required=True)
    w.add_argument('--extra', nargs='*')
    r = sub.add_parser('replay')
    r.add_argument('--rust', required=True)
    r.add_argument('--go', required=True)
    r.add_argument('--export', required=True)
    r.add_argument('--params', required=True)
    r.add_argument('--to', type=int, required=True)
    r.add_argument('--jobs', type=int, default=4)
    r.add_argument('--gomaxprocs', type=int, default=4)
    r.add_argument('--pow-dag')
    r.add_argument('--workdir', required=True)
    r.add_argument('--repeat', type=int, default=3)
    r.add_argument('--go-first', action='store_true')
    r.add_argument('--only', choices=['rust', 'go'])
    r.add_argument('--keep', action='store_true')
    r.add_argument('--label', default='', help='name of this binary variant in the results')
    r.add_argument('--out', required=True)
    m = sub.add_parser('summarize')
    m.add_argument('files', nargs='+')
    a = p.parse_args()
    {'build-go': cmd_build_go, 'pow': cmd_pow, 'replay': cmd_replay, 'summarize': cmd_summarize}[a.cmd](a)


if __name__ == '__main__':
    main()
