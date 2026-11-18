# Transaction signature pipeline benchmark

Measured 2026-11-07 on Apple M1 Pro, 10 logical CPUs.
Compiler versions: go version go1.24.5 darwin/arm64; rustc 1.95.0 (59807616e 2026-04-14).
Go reference: db42af74d31f25390236ffeca14cca2cba7de639.

The current pure-Rust implementation is consistently about 2.6x faster for
valid Qi signature pipelines in this corpus. The stock Go build is faster for
Quai ECDSA sender recovery. This is a comparison of the actual implementations
and crypto backends, not an isolated language experiment.

## Complete signature pipelines

Each operation decodes fresh Protobuf bytes. Qi operations then parse input
keys, perform unsorted/untweaked MuSig2 aggregation when needed, construct the
signing hash and verify the signature. Quai operations reconstruct the signing
hash and recover the ECDSA sender. Go signers are prepared outside timing;
fresh transaction objects prevent the sender cache from skipping verification.

| Pipeline | Workers | Go operations/s | Rust operations/s | Rust / Go rate |
| --- | ---: | ---: | ---: | ---: |
| Quai decode + ECDSA sender recovery | 1 | 11,685 | 7,143 | 0.61x |
| Quai decode + ECDSA sender recovery | 4 | 46,681 | 27,078 | 0.58x |
| Qi single-input signature pipeline | 1 | 5,851 | 15,535 | 2.65x |
| Qi single-input signature pipeline | 4 | 22,561 | 59,554 | 2.64x |
| Qi 2-4-input signature pipeline | 1 | 2,062 | 5,358 | 2.60x |
| Qi 2-4-input signature pipeline | 4 | 8,008 | 21,106 | 2.64x |
| Qi invalid-signature pipeline | 1 | 2,514 | 8,993 | 3.58x |
| Qi invalid-signature pipeline | 4 | 9,670 | 35,074 | 3.63x |

These are signature-pipeline operations/s, not full-node TPS. Inputs are the
frozen compatibility corpus: 82 recoverable Quai cases, 20 valid single-input
Qi cases, 59 valid 2-4-input Qi cases and 17 invalid-signature Qi cases.
ETX, unsigned Quai and malformed/rejected cases participate in preflight but
are not mixed into successful-signature throughput.

Invalid Qi results are separate because early-exit behavior differs. Many
negative fixtures contain zero signatures; the Rust signature parser may
reject before hashing/curve verification. That ratio is not a valid-transaction
speedup or a general malformed-traffic result.

## Component stages, one worker

Decode stages perform real decoding and type checks. Signing-hash stages
start from decoded objects and include signing-message serialization. The
aggregation stage includes public-key parsing and key aggregation but excludes
transaction decoding, signing hash and signature verification.

| Stage | Go us/operation | Rust us/operation | Rust / Go rate |
| --- | ---: | ---: | ---: |
| Quai decode | 1.621 | 0.902 | 1.80x |
| Qi decode | 15.941 | 8.834 | 1.80x |
| Quai signing serialization + hash | 1.559 | 0.730 | 2.14x |
| Qi signing serialization + hash | 7.545 | 1.542 | 4.89x |
| Qi multi-input public-key parsing + aggregation | 288.902 | 110.056 | 2.63x |

Stage ratios must not be multiplied. Some stages overlap work repeated inside
the current public APIs; complete-pipeline measurements determine their net
effect.

## Allocation measurements

Separate sequential passes use Go runtime MemStats and a separately compiled
Rust allocator-counting binary. The Rust timing executable has no counting
allocator. Counts include allocations/reallocations; bytes are gross tracked
heap requests, not retained memory or RSS. Go counters exclude C allocations,
so these are not whole-process memory comparisons.

| Pipeline | Allocation calls/op, Go / Rust | Tracked bytes/op, Go / Rust | Fewer tracked bytes in Rust |
| --- | ---: | ---: | ---: |
| Quai decode + ECDSA sender recovery | 72.1 / 35.4 | 3,974 / 1,176 | 70.4% |
| Qi single-input signature pipeline | 103.9 / 40.2 | 6,934 / 2,130 | 69.3% |
| Qi 2-4-input signature pipeline | 223.6 / 60.7 | 14,836 / 3,488 | 76.5% |
| Qi invalid-signature pipeline | 200.2 / 44.1 | 13,240 / 2,496 | 81.2% |

## Correctness and experiment controls

- 130 timed samples: five paired repetitions per case,
  with one or four application workers for complete pipelines.
- 10,856,960 timed operations validated: 660,480
  successful-signature pipeline operations, 181,760 invalid-signature
  pipeline operations, with the remainder in component stages.
- 54 separate allocation samples validate
  27,648 operations.
- 45,056 calibration operations also validate.
- Zero mismatches/errors. Every timed result is checked; checksums and operation
  counts agree across implementations.
- Startup preflight checks all 288 codec cases and 40 rejected inputs against
  frozen canonical bytes, signing messages, hashes, IDs, senders, aggregate
  keys and signature outcomes. The Rust checker performs 2,632 comparisons;
  the independent Go harness performs 2,056 comparisons.
- Calibration chooses a common fixed operation count per workload before the
  five timed rounds. Each implementation and worker count uses that same count.
  Order alternates, and timed workloads run sequentially.
- Four-worker throughput scales roughly 3.8-4.0x for both implementations on
  these independent operations. This is not parallel state execution.
- Source, binaries, inputs, commands and raw repetitions are retained.
  Shipping Rust source/binaries and Go tracked source remain unchanged.

## Backends and limits

Rust uses the installed pure-Rust k256 0.13.4 implementation. The stock Go build
uses CGO-backed ECDSA recovery and public-key helpers; Qi Schnorr/MuSig2 code is
the pinned btcec v2.3.2 implementation. A CGO-disabled Go control was attempted
but cannot build because the transitive Litecoin secp256k1 module requires C.
Its unmodified build failure is retained in build/go-pure-build.log; no
substitute implementation was inserted.

The shared host's one-minute load ranged from
6.23 to 8.24.
Observed ranges in the raw data are not statistical confidence intervals.
The small warm synthetic corpus repeats a limited set of keys, uses at most
four Qi inputs and short payloads, and does not represent production blocks.

No UTXO database/ownership checks, balances, fees, lock rules, node chain-ID
policy, execution, receipts, full block validation, RPC, P2P, disk or sync run
inside these measurements. CPU samples include command warmup and thread
setup, while internal wall timings exclude them; sampled RSS is not peak live
allocation. The larger whole-node effect remains unmeasured.

## Interpretation and reproduction

This confirms Qi signature processing as a useful optimization target in the
current Rust client. Multi-input aggregation is a substantial remaining cost.
The weaker Quai recovery result calls for profiling the pure-Rust recovery
backend before making broader speed claims.

The reusable development driver builds fresh isolated artifacts and reruns the
same measured workloads and validation:

    python3 reference/benchmark_transactions.py --go-quai ../go-quai

It uses cached pinned dependencies and writes a new results directory.
The default build and standalone distribution do not include these benchmark
executables or require Go.

This run's [raw summary](../../results/20261107T224009Z-transaction-pipeline/summary.json),
[contract](../../results/20261107T224009Z-transaction-pipeline/contract.json),
[preflight](../../results/20261107T224009Z-transaction-pipeline/preflight.json),
[environment](../../results/20261107T224009Z-transaction-pipeline/environment.json),
[verification](../../results/20261107T224009Z-transaction-pipeline/verification.json) and adjacent raw files
provide the evidence. Exact original preparation/build/run scripts are saved
under the run's harness directory, with source and development binaries.
