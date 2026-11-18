#!/usr/bin/env python3
"""Send the vectors of a native RPC fixture (scripts/gen_native_rpc_vectors.py)
to a running `quai-node rpc` and compare its answers with the recorded go-quai
answers. Optionally also ask the live Go node again (--go) and compare live.

    scripts/compare_native_rpc.py fixtures/native-rpc/zone-0-27406.json \\
        --rust http://127.0.0.1:8547 [--go http://127.0.0.1:9200] [--report FILE]

Prints the counts per method and every differing answer; exits 1 when an
"equal" or "head" vector differs or a "difference" vector does not give its
documented Rust answer. Only object member order is ignored.
"""
import argparse
import json
import sys
import urllib.request
from decimal import Decimal


def post(url, method, params):
    body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params})
    req = urllib.request.Request(url, data=body.encode(),
                                 headers={'content-type': 'application/json'})
    raw = urllib.request.urlopen(req, timeout=120).read()
    answer = json.loads(raw, parse_float=Decimal)
    return {k: answer[k] for k in ('result', 'error') if k in answer}


def same(a, b):
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('fixture')
    ap.add_argument('--rust', default='http://127.0.0.1:8547')
    ap.add_argument('--go', help='also query this live Go node (read-only methods only)')
    ap.add_argument('--report')
    args = ap.parse_args()
    doc = json.load(open(args.fixture), parse_float=Decimal)
    per = {}
    failures = []
    live = {'equal': 0, 'different': 0}
    for v in doc['vectors']:
        method = v['method']
        got = post(args.rust, method, v['params'])
        want = v['rust'] if v['compare'] == 'difference' else v['go']
        ok = same(got, want)
        go_equal = same(got, v['go'])
        row = per.setdefault(method, {'vectors': 0, 'equal_to_go': 0, 'documented_difference': 0,
                                      'failed': 0})
        row['vectors'] += 1
        if ok and go_equal:
            row['equal_to_go'] += 1
        elif ok:
            row['documented_difference'] += 1
        else:
            row['failed'] += 1
            failures.append({'method': method, 'params': v['params'], 'want': want,
                             'got': got, 'compare': v['compare']})
        if args.go and v['compare'] == 'equal':
            if not method.startswith('quai_') and method != 'net_version':
                raise SystemExit(f'refusing to send {method} to Go')
            if same(post(args.go, method, v['params']), got):
                live['equal'] += 1
            else:
                live['different'] += 1
    total = sum(r['vectors'] for r in per.values())
    equal = sum(r['equal_to_go'] for r in per.values())
    documented = sum(r['documented_difference'] for r in per.values())
    failed = sum(r['failed'] for r in per.values())
    width = max(len(m) for m in per)
    print(f'{args.fixture}: head {doc["head"]}, {total} vectors: {equal} equal to Go, '
          f'{documented} documented differences, {failed} failed')
    for m in sorted(per):
        r = per[m]
        extra = f', {r["documented_difference"]} documented' if r['documented_difference'] else ''
        extra += f', {r["failed"]} FAILED' if r['failed'] else ''
        print(f'  {m:<{width}}  {r["equal_to_go"]}/{r["vectors"]} equal{extra}')
    if args.go:
        print(f'live Go node, "equal" vectors asked again: {live["equal"]} equal, '
              f'{live["different"]} different')
    for f in failures[:20]:
        print('FAILED', f['method'], json.dumps(f['params'], default=str)[:160])
        print('  want', json.dumps(f['want'], default=str)[:400])
        print('  got ', json.dumps(f['got'], default=str)[:400])
    if args.report:
        with open(args.report, 'w') as out:
            json.dump({'fixture': args.fixture, 'head': doc['head'], 'per_method': per,
                       'live_go': live if args.go else None, 'failures': failures}, out,
                      indent=1, default=str)
    sys.exit(1 if failed else 0)


if __name__ == '__main__':
    main()
