# Standalone Rust RPC read speed

Measured 2026-11-05 against the pinned Go API reference. Rust is faster on logs,
roughly even on two receipt types, faster on ETX receipts, and slower on full
block reads in this run. Server CPU per request is substantially lower, while
p99 latency is worse on four of five workloads. These measurements support an
efficiency opportunity; they do not establish a broad execution-speed gain.

| Workload | Go calls/s | Rust calls/s | Rust / Go | p99 ms, Go / Rust | Server CPU ms/call, Go / Rust | Peak sampled RSS MiB, Go / Rust |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Full block | 2,325 | 1,915 | 0.82x | 26.82 / 43.03 | 0.249 / 0.044 | 33.94 / 3.08 |
| Quai receipt | 974 | 957 | 0.98x | 63.82 / 65.06 | 0.444 / 0.068 | 34.31 / 3.47 |
| ETX receipt | 1,270 | 1,479 | 1.16x | 36.76 / 41.88 | 0.389 / 0.063 | 34.39 / 3.44 |
| Qi receipt | 1,288 | 1,282 | 1.00x | 48.31 / 58.69 | 0.417 / 0.060 | 33.73 / 3.44 |
| Single-block logs | 8,297 | 11,140 | 1.34x | 9.26 / 7.73 | 0.092 / 0.022 | 36.48 / 3.73 |

All 180,366 timed responses passed comparison; zero failures.
There were 30 timed measurements: three two-second repetitions per workload
and implementation, eight concurrent requests, with implementation order
alternating each round. Throughput, CPU/call and p99 columns are medians of
per-run values. RSS is the largest sampled resident set per workload.

Both services ran on the same shared 10-logical-CPU macOS host. One-minute load
averages during the run ranged from 71.2 to 86.6, and some throughput ranges overlap
substantially. Treat the ratios and latency estimates as provisional, especially
the small receipt differences. The Go load client performs JSON parsing,
canonicalization and equality comparison on every result. Client CPU and host
scheduling can limit observed throughput; this run does not isolate a cause.

The Go baseline is a seeded API oracle using the pinned go-quai APIs, not a
running production hierarchy. Rust serves a three-block imported frozen view
with six receipts and two logs. Requests repeatedly target the same head block,
three receipts or its single-block logs; formatted expected JSON is about
6.4 KB for the block, 21-22 KB per receipt and 472 bytes for logs. This is a warm,
small working set, not a large or cold database workload. Architecture and data
representation differ, so the result cannot be attributed to Rust alone.

CPU is cumulative OS server-process CPU sampled with `ps`, excluding the load
client and the other idle service. Its window includes the 32 warmup calls
before each timed run; CPU/call divides by timed calls only. RSS is sampled
every 100 ms, not allocated bytes or total memory footprint. The tiny frozen
fixture does not predict full-node memory use.

Rust used the shipping standalone binary with no source change or rebuild for
this run. Its health counters stayed at zero upstream and freshness requests.
The Go service and Go load client were explicit offline test tools in a separate
reference copy; neither is a Rust runtime dependency.

Independent sync, consensus validation, ingestion, state execution, EVM speed,
P2P throughput and production transaction throughput remain unmeasured.
The next performance baseline should use a quiet host and a larger working set
with client CPU separated, before tuning the Rust response path.

Raw measurements, health snapshots, fixed inputs and source/binary checksums:
[results/20261105T172254Z-standalone-speed/summary.json](../../results/20261105T172254Z-standalone-speed/summary.json). Prior standalone correctness checks:
[results/rust-20261105T171612867652Z/summary.json](../../results/rust-20261105T171612867652Z/summary.json).

To repeat against an explicitly built isolated reference copy:

```sh
python3 reference/benchmark_standalone.py --reference-lab /path/to/isolated/reference-copy
```

The helper preserves each new run separately and updates
`results/latest-standalone-speed.json` only on success. Building a reference
copy is an explicit development step via `scripts/build_reference.py`.

## Follow-up Rust copy experiment

An isolated buffer-sharing candidate reduced server CPU/call but did not pass
the across-workload throughput gate. It was not promoted. See the
[paired Rust trial](../../results/20261105T173607Z-copy-trial/REPORT.md).

## Native transaction and allocation increment

[Current native results](NATIVE_ROUND_RESULTS.md) add transaction primitives and
a Rust-only profiler. The paired HTTP comparison is a separate run; historical
measurements above remain preserved.
