Same text as in [CANONICAL_READS.md](CANONICAL_READS.md); omitted here.

# Native read milestone results

Local validation on 2026-11-04. The implementation adds a real isolated go-quai launcher, published SDK acceptance tests, typed API scenarios and a Rust materialized block/receipt store.

## Canonical receipt/log experiment

The optional [canonical read path](CANONICAL_READS.md) passes 13 correctness checks,
37 strict comparisons and 16 SDK comparisons. Its separate benchmark validates
235,713 calls with zero failures, but strict live receipts achieve only 0.53–0.72x
Go throughput and single-block logs 0.10x. Two per-request freshness batches cost
more than the indexed lookup saves here. This is a failed performance improvement;
the feature remains disabled by default. Earlier frozen-read gains below do not
apply to this live path.

## Live ingestion increment

The live ingestion implementation adds continuous Rust polling, a durable bounded checkpoint,
rolling retention, automatic branch replacement and process-crash recovery. The
[live ingestion guide](LIVE_INGESTION.md) records its runtime controls, explicit
local allocation/activation fixture, current acceptance report and remaining gates.
The installed live suite passes **19 lifecycle/fault checks**, **12 SDK comparisons** and **12 strict RPC comparisons**, including ten mined local blocks and one signed Quai transfer. [Current live results](../../results/live-20261105T155636313798Z/summary.json) retain the raw evidence.

The read timings below belong to the earlier recorded build, before this increment;
they are not new measurements of the follower or its ingestion cost.

## Compatibility evidence

The latest run passes **120 SDK comparisons**, **59 strict RPC comparisons** and **9 state/outage/import scenarios**.

The acceptance suite checks the real local full node at genesis and after restart. Funded StateDB/EVM and typed-block scenarios separately exercise successful preparation flows, Quai/Qi/external transactions, receipts and logs. The latest result and its raw client reports are referenced by [latest-acceptance.json](../../results/latest-acceptance.json).

- Both npm SDK pins are tested: `1.0.0-alpha.57` and `1.0.0-alpha.58`. Reports retain their older runtime version constants separately.
- Strict RPC comparisons, source outage, branch replacement, orphan receipt handling, corrupt/truncated imports, identity changes and native-store restart are covered.
- The earlier 704 controlled-state comparisons and 280 fixture checks pass unchanged. Ten Rust unit tests, eleven Python test groups and Clippy pass.
- Existing Go HeaderChain rollback and Qi regressions pass; see [core-regression.json](../../results/core-regression.json).

An API scenario block is seeded and has not been mined or consensus validated. The real full-node run is a genesis/restart test. These are separate evidence classes, and no complete mainnet, mined-chain, extension-UI or consensus-replay claim follows.

## Initial measurements

The read benchmark validated **754,537 calls with zero failures**. Each cell is the median of three two-second runs with eight concurrent requests, warm connections and every response validated by the same Go client. All raw rounds are retained in [results/20261104T223819Z-native](../../results/20261104T223819Z-native/summary.json).

| Dataset and path | Calls/s | HTTP p99 | Server CPU ms/call | Sampled peak server RSS |
|---|---:|---:|---:|---:|
| Real node genesis block / Direct Go | 7,895 | 2.037 ms | 0.2722 | 883.1 MiB |
| Real node genesis block / Forwarding gateway | 9,643 | 1.660 ms | 0.2793 | 888.5 MiB |
| Real node genesis block / Hybrid | 12,710 | 1.438 ms | 0.0397 | 884.0 MiB |
| Real node genesis block / Frozen Rust | 13,032 | 1.359 ms | 0.0311 | 3.7 MiB |
| Typed block / Direct Go | 6,503 | 2.229 ms | 0.2474 | 34.7 MiB |
| Typed block / Forwarding gateway | 6,052 | 2.382 ms | 0.3969 | 40.4 MiB |
| Typed block / Hybrid | 8,043 | 2.059 ms | 0.0348 | 39.9 MiB |
| Typed block / Frozen Rust | 8,085 | 1.979 ms | 0.0337 | 4.5 MiB |
| Typed receipt-0x0 / Direct Go | 3,766 | 3.753 ms | 0.4047 | 34.9 MiB |
| Typed receipt-0x0 / Forwarding gateway | 4,011 | 3.497 ms | 0.4869 | 40.6 MiB |
| Typed receipt-0x0 / Hybrid | 4,030 | 3.561 ms | 0.4922 | 40.9 MiB |
| Typed receipt-0x0 / Frozen Rust | 5,137 | 2.935 ms | 0.0467 | 4.7 MiB |
| Typed receipt-0x1 / Direct Go | 4,135 | 3.364 ms | 0.3793 | 34.0 MiB |
| Typed receipt-0x1 / Forwarding gateway | 4,394 | 3.096 ms | 0.4616 | 39.5 MiB |
| Typed receipt-0x1 / Hybrid | 4,407 | 3.203 ms | 0.4605 | 40.3 MiB |
| Typed receipt-0x1 / Frozen Rust | 5,726 | 2.557 ms | 0.0454 | 4.7 MiB |
| Typed receipt-0x2 / Direct Go | 4,026 | 3.819 ms | 0.3789 | 34.0 MiB |
| Typed receipt-0x2 / Forwarding gateway | 4,295 | 3.484 ms | 0.4316 | 39.4 MiB |
| Typed receipt-0x2 / Hybrid | 4,423 | 3.332 ms | 0.4395 | 40.1 MiB |
| Typed receipt-0x2 / Frozen Rust | 5,658 | 2.755 ms | 0.0442 | 4.6 MiB |

For the real node's genesis-block hash query, hybrid throughput was **1.61x** direct Go and measured combined server CPU per call was **85% lower**. Go remains running and is included in hybrid CPU/RSS. This is a known immutable block lookup and serialization gain, not a chain execution result.

For the seeded typed block, hybrid throughput was **1.24x** Go. Frozen receipt lookup throughput was **1.36–1.41x** the scenario backend. Frozen receipt selection has an explicit fixed canonical view; those ratios cannot be applied to live receipt serving.

Hybrid receipt requests still forward to Go. Their measured server CPU cost was **16%–22% higher** than direct Go in these runs. The forwarding layer is not a universal cost reduction; native coverage and the actual traffic mix matter.

## Interpretation and limits

- Hybrid resource figures include the source Go process plus Rust. The source Go hierarchy still determines most memory use; a small standalone frozen service does not replace that requirement.
- Frozen-mode figures include Rust alone. The cost of producing/importing its export is outside the timed read loop.
- Process CPU comes from sampled OS process counters over the generator window, including warmup/startup around the timed requests. RSS is sampled and can miss peaks; it is not allocated bytes per operation.
- Percentiles are medians of per-run HTTP completion percentiles, not pooled percentiles. Client validation/decoding remains in request latency and can limit throughput; client CPU is retained in every raw record.
- Go/client and Rust worker settings, implementation paths and feature coverage differ. The experiment compares service implementations and materialization; it does not isolate the programming language.
- The shared macOS host, small data sets and short repetitions make this a scoped initial result. An idle Linux host, larger real traces, ingestion/reorg costs and sustained SLO testing remain qualification work.

## Reproduce

```sh
python3 scripts/build.py
npm ci --prefix sdk --ignore-scripts --no-audit --no-fund
python3 scripts/verify_next.py
python3 scripts/benchmark_native.py --rounds 3 --seconds 2
```

Use [NATIVE_READS.md](NATIVE_READS.md) for runtime modes, bounded export/import, partial history semantics and reload instructions. The benchmark records a separate source contract; the original bandit experiment and its historical results remain intact.
