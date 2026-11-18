> Historical Go-backed profile. The default build is now standalone Rust; see [README](README.md) and [the native plan](PLAN.md). Run these reference commands only inside the isolated lab created by `scripts/build_reference.py`.

# Native canonical receipts and indexed logs

The optional canonical read path serves positive `quai_getTransactionReceipt` results and covered `quai_getLogs` queries from Rust. Each candidate is checked against Go before and after the local lookup. Go remains responsible for validating the chain. These are RPC read indexes, not a native execution engine or an independent node.

## Enable and verify

```sh
python3 scripts/build.py
python3 scripts/run.py --mode hybrid --upstream "$QUAI_TEST_RPC" \
  --snapshot run/live.json --follow --native-canonical-reads
python3 scripts/verify_fresh.py
```

The option defaults to off and requires the loopback follower. Without it, the existing hybrid receipt/log forwarding behavior remains available. Frozen snapshot mode can also serve covered log queries with an explicit fixed canonical view.

For each eligible live request, Rust pins one indexed snapshot and checks an uncached batch of four Go reads: chain ID, node location, genesis block and latest block metadata. It computes the local result and repeats that batch. Both batches must identify the pinned chain and head, and the follower must still report a verified source. A head mismatch discards the candidate and forwards the original request to Go. An identity mismatch returns an availability error. Missing or malformed proof responses use Go fallback; a source outage cannot produce a stale live result.

These checks use **two HTTP batches and eight Go RPC operations** per successful native request. A native result therefore still depends on Go. The chain can advance after the final observation, as it can after another RPC read. The checks establish a view observed during the request, not a permanent freshness guarantee.

## Coverage and exact behavior

Only positive indexed receipts use the live native path. Unknown, orphaned or uncovered transaction hashes go to Go, including when an offline export describes complete history. This avoids inventing null results for pending or otherwise uncovered transactions.

The log index is built from structurally validated receipt logs in canonical block, transaction and log-index order. Import checks inclusion metadata, address/topic/data encoding, removed flags and contiguous block log indexes. Unknown log fields and repeated events remain intact. Indexes select candidates by address and topic position.

Covered filters include canonical `blockHash`, indexed number ranges, `latest`/`earliest` where covered, address strings/arrays, topic-position AND, within-position OR, null and empty wildcards. The parser follows Go's first-null behavior inside a topic OR list. Invalid or unsupported filters go to Go so its results and errors remain observable.

Go range filters append worker results in a nondeterministic block order. Rust serves empty results or results with one contributing block natively, preserving exact single-block order. Queries matching logs from several blocks go to Go. The acceptance harness records the actual forwarded response for these cases and checks it exactly, preserving multiplicity and ordering instead of sorting errors away.

The import limit is 200,000 canonical log records. Local queries scan at most 50,000 selected candidates, return at most 10,000 logs and obey the RPC response budget. Coverage/order/budget misses fall back to Go. Live reads do not use partial histories as complete negative indexes. Reorgs rebuild and atomically replace the receipt and log indexes together.

`/health.canonical_reads` reports native receipt/log hits, freshness HTTP/RPC attempts, fallback reasons and identity rejections. `upstream_calls` continues to count original forwarded requests; freshness probes and ingestion have separate accounting. A fallback is not a native hit.

## Evidence and performance

[Latest correctness result](results/latest-fresh.json) covers typed Quai/Qi/ETX receipts, address/topic filters, exact forwarding witnesses, partial windows, proof errors, source outage, response-ID ordering, same-height reorgs and a reorg between barriers. Both pinned Quais.js versions exercise high-level receipt and log decoding. A mined local Quai transfer uses the existing explicit allocation/activation fixture and verifies native receipt/empty-log reads and restart. Nonempty mined contract logs remain additional qualification work; the current nonempty-log matrix uses production Go API handlers over seeded data.

```sh
python3 scripts/benchmark_fresh.py
```

This is a separate recorded baseline: direct Go, Go plus a forwarding follower, and Go plus the strict native read follower. It uses a separate read-only client that validates every returned receipt/log result. The earlier reference/client and timed workloads are unchanged. All timed runs are sequential, with three two-second repetitions per workload and mode, eight concurrent requests and combined Go/Rust server CPU. The shared macOS host and seeded data limit the result to local warm reads. Ingestion remains active in both follower modes.

[Latest measurements](results/latest-fresh-benchmark.json) retain raw timings, validation counts, route counters, source/binary hashes, client CPU and sampled RSS. Strict per-request proof costs must be included when assessing the path; frozen-read gains from earlier experiments do not establish live-read speedups.

## Recorded result on 2026-11-05

The correctness suite passes 13 checks, 37 strict comparisons and 16 SDK comparisons. The benchmark validates 235,713 calls with zero failures. This strict prototype is a **performance regression** and remains opt-in, disabled by default.

| Workload | Direct Go calls/s | Forwarding follower calls/s | Strict native calls/s | Native / Go |
|---|---:|---:|---:|---:|
| Quai receipt | 2,202 | 2,355 | 1,166 | 0.53x |
| External receipt | 2,364 | 2,449 | 1,693 | 0.72x |
| Qi receipt | 1,980 | 2,443 | 1,304 | 0.66x |
| Single-block logs | 12,330 | 7,358 | 1,255 | 0.10x |

Native receipt server CPU per call is about 2.4x direct Go in this run; native log CPU is about 9.7x. Native counters confirm the measured requests were served locally after the barriers, with no original receipt/log forwarding. Probe and active-ingestion work is included in the combined Go/Rust server cost. The record is a small shared-host warm-read experiment, not a production estimate.

[Full measurements](results/20261105T162737Z-fresh-benchmark/summary.json) retain all 36 timed runs. The next performance experiment should amortize freshness barriers across a sealed group of requests while preserving the request-time checks; a time-based cache must not silently weaken this contract.
