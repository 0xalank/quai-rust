#!/usr/bin/env python3
"""Summarize a macOS `/usr/bin/sample` call graph per thread.

    /usr/bin/sample PID SECONDS -file OUT.txt
    scripts/profile_sample.py OUT.txt [--thread SUBSTRING] [--top N] [--frames NAME ...]

For each thread whose header contains SUBSTRING (default: the main thread),
it prints the samples in each function as the innermost frame (self) and
anywhere on the stack (inclusive), with the share of the thread's samples.
Samples whose innermost frame is a wait (psynch, kevent, mach_msg, ...) are
counted apart as idle, and the shares of the busy samples are given too.
"""
import argparse
import re
from collections import Counter

LINE = re.compile(r'^(?P<indent>[ +!:|]*)(?P<count>\d+) (?P<name>.+?)(?:  \(in (?P<image>[^)]+)\))?(?: \+ \d+)?(?:  \[.*\])?(?:  .*)?$')
IDLE = ('__psynch_cvwait', '__psynch_mutexwait', 'kevent', 'mach_msg', '__semwait_signal',
        '__select', '__read_nocancel', '__ulock_wait', 'semaphore_wait_trap', '__workq_kernreturn',
        '__wait4', 'poll', '__accept', '__recvfrom', '_pthread_cond_wait')


def clean(name):
    name = re.sub(r'::h[0-9a-f]{16}$', '', name.strip())
    return name


def threads(text):
    """Yield (header, [(depth, count, name)]) per thread of the call graph."""
    graph = text.split('Call graph:', 1)[1].split('Total number in stack', 1)[0]
    current, rows = None, []
    for raw in graph.splitlines():
        if not raw.strip():
            continue
        m = LINE.match(raw.rstrip())
        if not m:
            continue
        depth = len(m.group('indent'))
        if depth <= 4 and 'Thread_' in m.group('name'):
            if current is not None:
                yield current, rows
            current, rows = raw.strip().split(' ', 1)[1], []
            continue
        rows.append((depth, int(m.group('count')), clean(m.group('name'))))
    if current is not None:
        yield current, rows


def summarize(rows):
    """Self samples (innermost frame) and inclusive samples per function."""
    self_counts, incl = Counter(), Counter()
    stack = []

    def close(node):
        self_counts[node['name']] += node['count'] - node['children']

    for depth, count, name in rows:
        while stack and stack[-1]['depth'] >= depth:
            close(stack.pop())
        if stack:
            stack[-1]['children'] += count
        if name not in {n['name'] for n in stack}:
            incl[name] += count
        stack.append({'depth': depth, 'count': count, 'name': name, 'children': 0})
    while stack:
        close(stack.pop())
    return self_counts, incl


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('file')
    p.add_argument('--thread', default='com.apple.main-thread')
    p.add_argument('--top', type=int, default=30)
    p.add_argument('--frames', nargs='*', default=[])
    a = p.parse_args()
    text = open(a.file, errors='replace').read()
    for header, rows in threads(text):
        if a.thread not in header:
            continue
        total = rows[0][1] if rows else 0
        self_counts, incl = summarize(rows)
        idle = sum(c for n, c in self_counts.items() if any(n.startswith(w) for w in IDLE))
        busy = max(total - idle, 1)
        print(f'{header}: {total} samples, {idle} idle, {total - idle} busy')
        print('  self (busy share):')
        for name, c in self_counts.most_common():
            if any(name.startswith(w) for w in IDLE):
                continue
            print(f'    {c:7d} {100 * c / busy:5.1f}%  {name[:150]}')
            a.top -= 1
            if a.top <= 0:
                break
        if a.frames:
            print('  inclusive (busy share):')
            for f in a.frames:
                hits = [(n, c) for n, c in incl.items() if f in n]
                for n, c in sorted(hits, key=lambda x: -x[1])[:8]:
                    print(f'    {c:7d} {100 * c / busy:5.1f}%  {n[:150]}')


if __name__ == '__main__':
    main()
