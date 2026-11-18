# Native transaction and RPC increment

Completed locally on 2026-11-05. Native transaction primitives, a Rust-only
profiler and the measured allocation change are installed. Replaced source and
shipping artifacts are preserved under reference-artifacts/native-round-20261105T192859Z/before.
No commit, push or deployment was performed.

## Native compatibility

The primitive crate reconstructs Quai/Qi/ETX Protobuf, canonical bytes and
routed IDs, encodes signing messages and typed RLP, recovers Quai ECDSA senders,
and checks Qi BIP340 signature mathematics using unsorted, untweaked MuSig2
input-key aggregation. Cryptography uses the pure Rust RustCrypto k256/SHA-2/
SHA-3 implementations; [attribution](../../crates/quai-primitives/NOTICE.md) is shipped.

The oracle produces 288 codec cases and 40 rejections. The native checker
passes 2,632 transaction comparisons plus 1,711 header/target comparisons:
**4,343 total**. Coverage includes contract creation, integer normalization,
compressed/uncompressed keys, repeated keys, multi-input aggregation, invalid
signatures, missing fields, ETX ledger bits, unknown Protobuf fields and signing
message independence from work fields. See the exact revision and btcec 2.3.2
source hashes in [transaction provenance](../../fixtures/protocol/transactions-source.json).

The supported domain is one MiB of Protobuf, 256-bit magnitudes, 128 KiB data
and 4,096 list entries. Chain-ID policy, UTXO ownership, value/fees/locks and
state transitions require a native validator. Explicit origin is not
authentication; ETX signing data is empty. Qi typed RLP omits work fields and
is not used as the transaction-ID preimage. Typed-RLP decoding remains open.

## Handler performance

The profiler calls the handler without sockets or Go. Each frozen expected
response is checked structurally before timing, and every timed response must
match those validated bytes exactly. Timing includes byte comparison and
excludes external client parsing/validation. Separate allocation samples count
allocations/reallocations and gross requested bytes, not live memory or RSS.

The accepted change borrows the request ID and semaphore permit, avoids a
temporary JSON-RPC version allocation, and reserves room for the complete
result and closing brace before copying the payload. Store ownership, errors,
limits, notifications and batch order retain their accepted semantics.
The earlier shared-buffer candidate remains rejected and is not installed.

| Workload | Baseline us/request | Updated us/request | Handler rate ratio | Allocation calls, baseline / updated | Gross bytes, baseline / updated |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full block | 1.250 | 1.129 | 1.11x | 16 / 13 | 22,189 / 11,636 |
| Quai receipt | 4.065 | 2.727 | 1.49x | 18 / 15 | 88,904 / 45,063 |
| ETX receipt | 4.150 | 2.789 | 1.49x | 18 / 15 | 85,208 / 43,215 |
| Qi receipt | 4.084 | 2.249 | 1.82x | 18 / 15 | 84,788 / 43,005 |
| Single-block logs | 1.713 | 1.784 | 0.96x | 25 / 23 | 4,194 / 4,184 |
| Chain ID | 0.571 | 0.574 | 0.99x | 13 / 11 | 1,074 / 1,064 |
| 16-receipt batch | 97.027 | 51.342 | 1.89x | 316 / 268 | 2,815,033 / 2,113,151 |
| Mixed batch | 13.011 | 8.829 | 1.47x | 74 / 65 | 207,946 / 155,276 |

Three alternating repetitions used 100,000 timed requests per case plus warmup
and separate allocation samples. **4,818,480 requests validated**,
zero failures. The geometric mean handler rate ratio is
1.362x. Logs were 4.0% slower and chain ID roughly flat,
within the predeclared 5% per-workload time guard. Allocation calls fell for
every case. Receipt gross allocated bytes fell about 49%; the 16-receipt batch
fell about 25%.

The first request-only candidate missed its time guard and was not promoted.
Its [raw result](../../results/20261105T194301Z-handler-profile/summary.json) is kept.
The [accepted longer run](../../results/20261105T194535Z-handler-profile/summary.json) records guards, hashes,
repetitions and load. These are instrumented warm-handler measurements on a
shared Mac, not production capacity.

## Separate HTTP comparison

The unchanged offline load client validates every response in three paired
two-second repetitions at concurrency eight. Both services are Rust, with no
Go server or upstream. The Go load generator is an external offline test tool.

| Workload | Previous Rust calls/s | Updated Rust calls/s | Change | p99 ms, previous / updated | Server CPU/call change |
| --- | ---: | ---: | ---: | ---: | ---: |
| Full block | 7,632 | 8,130 | +6.5% | 2.733 / 2.139 | -6.1% |
| Quai receipt | 5,210 | 5,060 | -2.9% | 3.029 / 3.322 | -6.5% |
| ETX receipt | 5,878 | 5,869 | -0.1% | 2.601 / 2.668 | -7.8% |
| Qi receipt | 5,859 | 5,844 | -0.2% | 2.569 / 2.567 | -4.8% |
| Single-block logs | 49,125 | 49,881 | +1.5% | 0.658 / 0.653 | -3.0% |

All **886,763 measured responses pass**, zero failures and zero Rust
upstream requests. Geometric mean HTTP throughput ratio:
1.009x. This does **not** pass the historical gate requiring at
least 3% improvement on every workload. All coarse p99 guards pass. The handler
change is accepted under its separate handler/allocation gate, without an
across-the-board HTTP speed claim.

Host load and external client work affect these rates. Even the unchanged
baseline ran much faster than in the earlier heavily loaded session, so those
absolute rates cannot be counted as an improvement. Server CPU excludes the
client and includes warmup; p99 is the median of per-run percentiles.
See [HTTP raw results](../../results/native-round-20261105T192859Z/transport-trial/summary.json).

## Verification and next gates

21 Rust tests, formatting, Clippy across all targets/features, 70 RPC
comparisons, 16 pinned SDK comparisons and eight standalone checks pass.
The isolated development reference profile also passes 280 controlled fixture,\nJSON-RPC, timeout/backpressure and gateway checks. Its Go fixture is an offline\ntest tool, excluded from the default distribution. See [reference checks](../../results/native-round-20261105T192859Z/isolated-reference-checks/conformance.json).\nThe reduced-source Go-denied build passes both corpora with zero Go command
attempts, no Go files and no checkout. Distribution remains two Rust binaries
with attribution, capabilities and checksums.

The [plan](PLAN.md) keeps typed-RLP decoding, complete block/body/receipt
commitments, serial replay/execution, auxiliary PoW, durable validated state,
P2P/sync and full wallet/RPC behavior open. The imported frozen view remains
explicitly unverified. EVM and full-node speed remain unmeasured.

[Acceptance](../../results/latest-rust.json),
[Go-denied build](../../results/rust-no-go-build.json),
[validation log](../../results/native-round-20261105T192859Z/final-round-validation.log),
[round verification](../../results/native-round-20261105T192859Z/verification.json).
