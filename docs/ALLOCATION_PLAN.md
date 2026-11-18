# Allocation and transaction performance work

Audited 2026-11-08 against the unchanged native source and the
[2026-11-07 pipeline benchmark](../TRANSACTION_PIPELINE_RESULTS.md).
**69–76% lower tracked heap bytes is an improvement.** It means fewer bytes were
requested from the tracked allocator per operation, not lower performance.

| Valid Qi pipeline | Go tracked bytes/op | Rust tracked bytes/op | Reduction | Rust allocation calls/op |
| --- | ---: | ---: | ---: | ---: |
| Single input | 6,934 | 2,130 | 69.3% | 40.2 |
| 2–4 inputs | 14,836 | 3,488 | 76.5% | 60.7 |

These are separate allocation passes: gross requested heap bytes, including
reallocation, not retained RAM or peak RSS. Go counters exclude C allocations.
The corresponding timed valid Qi pipelines were about 2.6x faster in Rust on
this small warm corpus. Whole-client memory/TPS impact remains unmeasured.
The pure-Rust Quai ECDSA pipeline was slower: 0.58–0.61x the stock Go rate.
Allocation wins alone do not explain or fix the cryptographic recovery cost.

## Prioritized experiments

All opportunities below are **proposed, unimplemented and unmeasured** in this
round. Keep native code, benchmark inputs, oracle and acceptance contracts
independently pinned. No numerical future speedup is promised.

| Order | Change to try | Source evidence and expected effect | Required invariant |
| --- | --- | --- | --- |
| 1 | Retain a validated public-key representation plus canonical compressed bytes once per input | `transaction.rs` decodes SEC1, stores expanded bytes, then signing serialization and aggregation parse/encode again; avoid repeated curve parsing and temporary encodings | Preserve rejection rules, reference uncompressed/canonical encodings, unsorted untweaked MuSig2 and exact IDs/signing bytes; representation stays private and immutable |
| 2 | Stream the key-list/coefficient hashes and use fixed-size key/scalar scratch | `qi_aggregate_key` builds per-key Vecs, `encoded.concat()`, and a new coefficient-message Vec; reduce allocation count without changing hash order | Duplicate-key/second-distinct-key behavior, infinity handling, scalar reduction and tagged hashes remain identical |
| 3 | Encode canonical signing messages without cloning a second nested owned Protobuf tree | `proto(true).encode_to_vec()` clones fields and builds temporary lists; try borrowed encoding or a bounded reusable output buffer | Byte-for-byte normalization, optional-field presence, lengths and signing hashes match the oracle; whole-message consensus rules remain unchanged |
| 4 | Use bounded worker-owned scratch and appropriate fixed-size values | Small integer/key Vecs and repeated list construction create memory traffic; profile actual allocation sites before changing layout | No shared mutable scratch, stale data, lifetime escape or unbounded retained capacity; preserve exact magnitude semantics |
| 5 | Profile pure-Rust ECDSA recovery and Qi curve math separately | Quai decode and signing serialization/hash are already faster, while full recovery is slower; investigate backend arithmetic, inversion/multiplication and redundant work | Keep pure-Rust shipping policy, full signature/curve checks and all malformed-input behavior; inspect generated profiles before claiming the bottleneck |

A representation cache cannot be allowed to go stale if fields are mutated.
Do not cache transaction validity across different state, chain context or fork
rules. The existing bounded math verifier is not a UTXO validation cache.

## Acceptance and measurement

- Rebuild and pass the 21 Rust tests, 4,343 bounded protocol comparisons,
  frozen RPC/SDK checks and isolated reference checks before timing changes.
  Preserve rejected/malformed cases and exact outputs; add boundary cases for
  any changed representation without replacing the frozen benchmark contract.
- Run timing with the uninstrumented executable. Count allocations in a
  separately compiled pass. Retain source/binary/input hashes and raw paired
  rounds, worker counts and shared-host load. Keep timing sequential.
- Compare complete valid signature pipelines, not multiplied stage ratios.
  Keep invalid-signature early-exit cases separate. Reject a change that merely
  skips work or transfers allocation outside the measured ownership boundary.
- Establish a separate reviewed workload for larger valid Qi input counts,
  diverse keys, realistic payloads and mixed blocks. The current synthetic
  corpus repeats seven test keys, uses at most four inputs and excludes disk,
  state/UTXO ownership, fees, locks and execution.
- Once state/sync exists, measure complete block validation and live RPC/P2P
  workloads with CPU, peak RSS, queue retention, disk costs and p95/p99 latency.
  Fewer allocated bytes do not establish lower retained RAM or faster sync.

The next network-critical work remains full protocol/commitments, serial state
and consensus, durable storage and controlled P2P. Use allocation experiments as
small, separately validated changes, rather than delaying those missing systems.
See [network gates](../NETWORK_READINESS.md) and [the source map](../PORTING_MAP.md).
