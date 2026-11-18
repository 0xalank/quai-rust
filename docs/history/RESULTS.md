# Initial RPC experiment

Run on 2026-11-03 local time (2026-11-04 UTC), on a shared macOS ARM64 development machine. This tests synthetic HTTP RPC handling, not a synced Quai node.

The original benchmark build is retained in the per-run environment files below.
`results/build.json` describes the current compatibility build. See
[controlled-state verification](../../compatibility/CONTROLLED-STATE.md) for later
correctness work; the performance numbers here have not been rerun.

## What passed

- Five Rust unit tests, `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings`.
- 280 HTTP integration and differential checks across baseline and optimized configurations, using the adjacent go-quai RPC package as the reference.
- Eight finite UCB1 tuning trials and two held-out workload rounds. Every timed response was validated.
- Three longer payload comparison rounds, rotating server order; zero failed requests.

## Longer payload measurements

Medians of three runs. Four concurrent HTTP requests, a synthetic payload containing 1 KiB of data, and the same Go load generator for both servers. Each round sends 65,536 single-call requests and 4,096 batches of 32 calls per server.

| Workload | Go reference calls/s | Rust baseline calls/s | Selected Rust calls/s | Selected / Go | Selected / Rust baseline |
|---|---:|---:|---:|---:|---:|
| Single call | 17,169 | 21,714 | 23,123 | 1.35x | 1.06x |
| Batch of 32 | 49,098 | 87,933 | 96,579 | 1.97x | 1.10x |

| Workload | Go request p95 | Selected Rust request p95 |
|---|---:|---:|
| Single call | 0.493 ms | 0.345 ms |
| Batch of 32 | 3.861 ms | 2.218 ms |

Batch latency is the time to complete the whole HTTP request, not amortized per-call latency. The table gives medians of per-run p95 values, not a pooled p95.

Maximum sampled server RSS across these rounds was 4.94 MiB for selected Rust and 23.86 MiB for the Go reference. Sampling can miss peaks; these small fixture processes do not represent real-node memory.

## What the tuner selected

`workers=4`, `batch_concurrency=8`, `encoding=raw`. The Rust baseline uses two workers, serial batches, and Value-based fixture encoding. Go uses `GOMAXPROCS=8`. The raw encoder reuses serialized immutable fixture fields while serializing request IDs and sequence values for each response.

Held-out geometric mean throughput ratios versus the Rust baseline were 1.89x and 1.98x. This aggregate is dominated by the explicit delay workload: overlapping independent waits improved that workload by about 7.7x. Short ordinary-payload holdout measurements varied; the longer comparison above is a separate post-selection diagnostic.

The measured configuration passed the documented coarse latency and sampled-memory guards and was written to `configs/recommended.json`. Server defaults remain the baseline. This is a bounded multi-armed bandit experiment, not neural policy training.

## Interpretation

The prototype is a useful starting point for RPC experiments: serialization reuse and scheduling are measurable, behavior is checked against go-quai, and optimization records can feed a later controller. These results do not establish production gains from replacing a Go node or putting a forwarding gateway in front of one.

The Go reference includes the existing RPC framework; the smaller Rust service implements selected methods. The configurations and implementations differ, so the ratios cannot isolate the effect of the language. The busy host and three repetitions do not support a statistical confidence claim. There is no real database, EVM, chain synchronization, P2P, TLS, WebSocket or live-node measurement here.

The next useful test is a frozen trace of actual Quai reads, including responses tied to block hashes, followed by a native Rust implementation of one selected read path and reorg checks.

## Evidence

- [Tuning summary](../../results/20261104T030019Z/summary.json) and adjacent per-trial records.
- [Longer comparison](../../results/20261104T030159Z-compare/summary.json) and adjacent per-round records.
- [HTTP conformance checks](../../results/conformance.json).
- [Build versions and source/binary hashes](../../results/build.json).
- [Frozen benchmark contract](../../fixtures/contract-sha256.json).
- Reference go-quai revision: `db42af74d31f25390236ffeca14cca2cba7de639`.
