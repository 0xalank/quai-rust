> Historical sidecar roadmap, retained for reference. The active native-client plan is [PLAN.md](../PLAN.md).

# Quai Rust build and compatibility plan

Build a useful RPC service first, then move selected reads into Rust while go-quai continues to validate the chain. A standalone validating client is a later track with its own execution and networking gates. This sequence lets wallets and applications exercise the real interface early and makes each performance change independently testable.

The first product target is a state-processing zone RPC endpoint. Prime and region contexts need explicit profiles because method availability and behavior differ. The target network and node configuration must be recorded before capturing chain fixtures.

The optional [canonical receipt/log prototype](CANONICAL_READS.md) now adds strict freshness barriers and native indexed queries. Multi-block ordering, gaps and unsupported inputs preserve Go fallback; sustained operational gates remain open.

## Latest implementation

The local full-node launcher, pinned SDK acceptance jobs, typed block/receipt/log
scenarios, and first materialized Rust read store are implemented. The
[read guide](NATIVE_READS.md) records their scope and commands. Hybrid mode serves
known immutable block hashes natively; live receipt and latest-state requests
continue through Go. Frozen exports support receipt selection and atomic branch
replacement. Complete release gates below still require broader full-node
coverage and operational qualification.

## Implementation status

The controlled-state API harness, strict comparator, local full-node launcher,
two pinned SDK jobs and first materialized read paths are implemented. The next
increment adds a local mined-chain fixture and automatic bounded Rust ingestion
with durable progress, rollback and crash recovery; see [live ingestion](LIVE_INGESTION.md).
The original benchmark and controlled API evidence remain separate. Full M0/M1,
wallet/event, native execution and production qualification gates remain open.

## Current starting point

The starter has an HTTP fixture service, a read-only forwarding gateway, 5 Rust unit tests, 280 fixture/integration checks, and a finite configuration tuner. Its 31 allowlist entries are forwarding permissions, not evidence of 31 compatible chain methods. The initial performance results in [RESULTS.md](RESULTS.md) are synthetic.

The [compatibility inventory](compatibility/README.md) seeds 67 candidate method entries from the Rust gateway, selected Go service declarations and the local Quais.js request mapping. It is not the complete runtime RPC surface. [methods.json](compatibility/methods.json) records exact revisions, source locations, source hashes, current status and empty case lists that still need evidence.

Start against go-quai `db42af74d31f25390236ffeca14cca2cba7de639`. The inspected Quais.js checkout reports `1.0.0-alpha.57`; the inspected Pelagus checkout reports `1.0.64` and depends on Quais.js `1.0.0-alpha.58`. Pin both SDK variants in client tests. These are local source snapshots, not a claim about the newest releases.

## Compatibility findings that shape the work

1. **A method name is already wrong.** The gateway permits `quai_getOutpointsByAddressAndRange`; Go declares `GetOutPointsByAddressAndRange`, exposed as `quai_getOutPointsByAddressAndRange`. Method names are case-sensitive. Correct the canonical entry and add a regression case before claiming that read works.
2. **Important client reads are missing.** Add explicit coverage for chain discovery, node location, nonce lookup, `quai_createAccessList`, `quai_estimateFeeForQi`, locked balances, conversions and outpoint deltas. Allowlisting a method is only the first step.
3. **Error text affects client behavior.** The current fixture comparator drops messages. Quais.js uses wording to classify missing transactions, insufficient funds, nonce failures and replacement failures. Compare code, data, message and the resulting SDK error classification.
4. **Qi reads have different state semantics.** The current Go `quai_getBalance` implementation rejects historical Qi balance queries. Outpoint deltas can span competing branches. Preserve those rules instead of making them look like ordinary account reads.
5. **Ethereum aliases need their own cases.** `eth_*` and `quai_*` register different blockchain API implementations. Do not assume their DTOs, address handling or receipt fields are interchangeable. Confirm runtime exposure of `web3_clientVersion`; the inspected services declare `quai_clientVersion` and `eth_clientVersion`.
6. **Block tags and HTTP details need profiles.** The inspected parser accepts `earliest`, `latest` and `pending`; do not silently map unsupported tags such as `safe` or `finalized` to `latest`. The starter's notification HTTP status and strict request validation also differ from some Go behavior.

The outpoint spelling and initial gateway gaps now have implementation and controlled-state tests. Remaining findings continue to guide the full-node and client acceptance work; see the current evidence above. Source anchors are collected below and in the inventory.

## Milestones and completion gates

| Milestone | Deliverable | Required evidence before moving on |
|---|---|---|
| M0 Reference contract | Runtime method inventory, pinned reference/configuration, versioned schemas, comparison policy and known differences | Every method in the chosen v0.1 profile has a real registration, exact spelling, operation class, parameter/error cases and an evidence owner. Missing cases remain visibly uncovered. |
| M1 Compatible HTTP reads | Expand the explicit gateway profile; add the chain-backed oracle, corpus recorder and SDK tests | All v0.1 cases pass against a controlled go-quai node, including errors and wrong-context calls. Quais.js read and transaction-preparation flows work with both pinned SDK versions. |
| M2 Wallet and event compatibility | HTTP filters, WebSocket subscriptions and a separately enabled relay for externally signed transactions | Filter expiry, reconnect, unsubscribe, slow-consumer handling and reorg events pass. Pelagus read/prepare/sign/submit/receipt flows pass on a controlled test chain. Submission is disabled in the read-only profile. |
| M3 First native read path | Rust block/receipt types and a separate local store, initially serving immutable block-hash reads | Go/Rust serialization and results match across Quai, Qi and external transactions. Reorg, restart, incomplete import and stale-head tests pass. These requests can be served without a Go RPC round trip. |
| M4 Indexed reads and operational pilot | Log index, followed by UTXO/outpoint indexing, with rollback and explicit freshness | Equivalent query results, correct chain transitions and crash recovery; sustained workload and shadow gates below pass. A limited pilot has a tested route back to Go. |
| M5 Native execution | Quai-specific execution/state transition engine, initially tested offline | Exact full-block replay of consensus outputs across the declared fork/test matrix, including rejected blocks. Generic EVM test success alone cannot close this gate. |
| M6 Independent node | P2P, sync, transaction pool, chain selection and all supported validation rules | Independent sync/reorg/restart tests and cross-client consensus tests pass on a controlled network before public deployment is considered. |

M0–M1 define the first usable release. M2–M4 make it useful as RPC infrastructure. M5–M6 are a substantially larger client project; calendar estimates should follow the RPC work and replay-harness findings.

## Establish the reference contract

Capture the Go commit and tracked diff, module/toolchain locks, network/genesis identity, chain ID, node location/context, state-processing and pruning settings, active fork rules, RPC gas/fee limits, HTTP/WS module flags and endpoint limits. Capture the same metadata for every fixture generation and benchmark run. A Git hash alone does not identify node behavior.

Export registrations using Go's actual callback eligibility and naming rules. `rpc_modules` can help identify enabled namespaces but is not a complete method/schema inventory. Combine registration evidence with source signatures and controlled endpoint probes. Do not expose everything marked `Public`: public services include methods that mutate state or have operational effects.

Define two explicit profiles: the initial HTTP read/simulation profile and the later wallet/event profile. Keep account management, server-side signing, mining control, admin/debug and broad transaction-pool APIs outside those profiles unless a concrete integration requires them. Wallet signing APIs in SDK code may target a wallet provider; their presence does not require the node to hold keys.

For each method record parameter order and optionality, number and hex encoding, null/omitted/empty distinctions, result shape, error behavior, applicable context, state dependence, transport, limits, current implementation route and fixture IDs. Track proxy compatibility and native implementation coverage separately.

## Build a comparison harness that catches real failures

The reference harness must run actual API handlers over controlled chain state. Extend beyond `reference/go/main.go`, whose current handlers return synthetic data. Generate local chain scenarios and retain immutable block/state exports for offline replay. Captured production reads can later improve workload realism without becoming the only source of expected behavior.

Every corpus case should identify the reference revision/profile, request, expected response, chain location, relevant block and parent hashes, state snapshot, scenario and comparison rule. Separate valid, invalid and boundary cases. Freeze a training corpus and a validation corpus split by block/scenario/client flow; repeated copies of the same block are not independent validation.

Compare parsed JSON exactly where semantics require it. Ignore object-key order. Preserve array order and duplicate multiplicity unless a method-specific contract justifies another rule. JSON-RPC batch responses can be correlated by ID under the [JSON-RPC specification](https://www.jsonrpc.org/specification); handle duplicate-ID test cases explicitly rather than overwriting them in a map. Preserve large IDs and quantities without floating-point conversion.

Compare error codes, data and client-visible message behavior. Record every normalization rule with a reason and a test; do not discard all errors, logs, unknown fields or volatile values to make results match. Current Go range-log collection can append results from concurrent workers, so characterize ordering and SDK expectations before choosing a log comparison policy. Any order-independent comparison must retain duplicates and expose order differences separately.

Run deterministic comparisons against the same state. Pin block hashes where each API supports them. For current-head-only Qi reads, pending transactions, estimates without block selectors and other mutable methods, use a paused controlled chain or an explicit state barrier. Live `latest`/mempool responses collected at different moments are not automatically implementation mismatches. A moving-head comparison must identify the race or use a defined invariant; never hide genuine stale data behind a broad tolerance.

The corpus needs these scenario families:

- Request framing, content types, batches, notifications, malformed JSON, IDs, limits, disconnects, deadlines and overload errors.
- Empty/missing blocks and transactions, hash-only/full transaction blocks, pending/mined/failed receipts, outbound ETXs and unknown fields across versions.
- Account and Qi address scope/checksums; wrong zone/context; historical state unavailable; current-block-only Qi balances; locked/spent outputs and denomination boundaries.
- Log topics and address combinations, empty/missing ranges, filter lifecycle, reorg removals and event backfill after reconnect.
- Contract return/revert data, access-list enforcement, gas/fee estimation, state overrides, Qi fee inputs and conversion behavior.
- Competing chains, reorgs at several depths, gaps, duplicate imports, restarts, crash during import and index rollback.

Use existing Go security/regression tests as scenario sources, including outpoint range overflow, malformed Qi encodings, ETX destination/value checks and EVM snapshot reversal. Add independently specified invariants as well: oracle agreement alone could reproduce an existing defect. Track an observed Go defect as an explicit versioned compatibility decision, never a silent behavior change.

## Client tests and rollout

Run the same scripts with only the RPC URL changed: direct Go, Rust forwarding to Go, and each enabled native Rust route. Cover both the pinned local Quais.js checkout and Pelagus's pinned dependency. Add the Ethereum provider used by Pelagus to test the declared `eth_*` subset.

Start with discovery, balance, block/receipt lookup, contract reads, nonce/access-list/fee preparation and Qi outpoint discovery. M2 adds contract and Qi sends using test keys on a controlled chain, receipt polling, filters, subscriptions, chain routing and reconnect. Confirm that the wallet shows the same actionable errors.

Shadow only eligible reads: Go serves the user while Rust computes a comparison result. Never mirror a transaction submission or subscription allocation as if it were a stateless read. Signed-transaction submission needs one destination, known-hash reconciliation and explicit ambiguous-timeout behavior; no blind automatic replay. Filters and subscriptions need connection/session ownership and bounded recovery.

The proposed gate before a limited native-read pilot is a 24-hour shadow run with at least one million comparable reads, zero unexplained semantic mismatches, zero correctness failures in the deterministic corpus, and successful restart/reorg/fallback exercises. These are future acceptance targets, not achieved results or a proof of universal compatibility. Roll out per method/location and preserve Go fallback for uncovered state and index gaps. Measure fallback frequency; a fallback must not count as a native Rust success.

## Move data into Rust without sharing a live database

Keep transport and request dispatch separate from typed Quai data and the read backend. A small layout is sufficient: `quai-rpc` for transport, a future `quai-types` for codecs, a backend interface for Go/native reads, and a future `quai-store` for immutable data and indexes. Put chain-backed comparison tooling beside the current fixture harness. Add an execution crate only when M5 begins.

Use a versioned snapshot/export or canonical-delta feed from the Go node into a separate Rust store. First define what the feed guarantees, how completeness is verified and how a checkpoint is resumed. Do not assume arbitrary RPC queries contain all state needed to reconstruct balances or UTXOs, and do not open the running Go database from a second independent writer.

Store block data by hash and maintain an atomic canonical-height mapping. Track the indexed head, parent links, schema version and undo information. A reorg rolls back derived data before exposing the new canonical head. If the store is behind, missing a range or unable to find a common ancestor, use the documented fallback or return a precise availability error; never label stale state as `latest`.

Start with block/receipt reads because their dependencies are narrower. Then profile indexed logs; the inspected Go range path visits blocks, making it a concrete candidate to evaluate. UTXO/outpoint indexing follows with spent-state, lock and branch-delta tests. Distinguish a cached upstream JSON response from a Rust implementation of the data/query semantics, even when both improve throughput.

## Performance and learning gates

Keep the current synthetic suite as a microbenchmark. Before optimizing production paths, establish a workload mix from real clients and measure direct Go, Rust proxy plus Go, and native Rust plus its Go/feed dependencies. Measure total system CPU, memory, disk and network work as well as response latency; moving work into another process does not make it disappear.

Use a dedicated Linux host, fixed CPU/resource budgets, the same chain snapshot and client load. Separate cold reads, warm reads, cache hit/miss rates and index-build cost. Run at least five paired rounds of at least 60 seconds, rotate order, and include offered-load/overload tests that report queueing and dropped requests. Record p50/p95/p99, throughput, errors, memory growth, disk footprint and head lag by method, not only an aggregate score.

A proposed promotion target is at least 20% lower total CPU per successful read at the same workload, or at least 25% higher sustained throughput at the same resource budget, with no greater than 5% p99 regression, no correctness regression and unchanged freshness/error guarantees. The measured variability must support the claimed gain. Freeze workload-specific SLOs and memory/lag budgets before the run; these thresholds are planning targets, not benchmark promises.

Keep correctness outside the learner's control. It may tune concurrency, buffer sizes, batching, serialization, index layout and an approved cache policy. It may not edit the oracle, fixtures, comparator, fork rules, resource gates or result accounting. Reject candidates for mismatches, stale data, changed failure behavior, resource overruns or dropped work before assigning a speed reward. Evaluate every important method separately so a fast delay benchmark cannot conceal a slower real workload.

Version any stronger production reward separately from the existing sealed experiment. Preserve its old results and contract. Record source/build/config hashes and independent validation results for each candidate. Use the existing bandit until the environment and measurement cost justify a more complex RL controller.

## Native execution and independent validation

Before choosing an EVM implementation, define exact Quai compatibility requirements: custom `ETX`, `ISADDRINTERNAL` and `CONVERT` opcodes; gas and fee rules; address and shard semantics; access-list checks; precompiles; creation behavior; and transaction types Quai, Qi and external transactions. Consensus also depends on UTXO, lockup, conversion, ETX and reward effects outside EVM execution.

Replay complete blocks from identical pre-state on Go and Rust. Compare all consensus commitments, account and UTXO state, receipts/logs, gas and fees, outbound/inbound ETX effects and acceptance/rejection. Include immediately before, at and after every relevant activation, with the correct prime/region/zone height context. Begin with serial execution. Parallel execution comes only after equivalence and uses state isolation, conflict handling and deterministic merge; declared account access alone is not a complete dependency model.

An independent node then needs exact wire/encoding/hash compatibility, peer negotiation and gossip, sync validation, fork selection/PoEM, historical/current PoW engines and workshares, transaction pool rules, persistence and recovery. Preserve third-party attribution and review dependency/license constraints when incorporating implementation code. Consensus divergence is a stop condition even if the Rust run is faster.

## First implementation batch

1. **C01 Reference profile and method registry.** Create a reproducible controlled-node launcher and registry export. Complete v0.1 rows and their schemas/cases. Select the initial chain/location/configuration and record SDK pins. Exit: a new developer can reproduce the reference and see exactly what is claimed.
2. **C02 Gateway compatibility gaps.** Fix the outpoint method spelling, add the selected read/simulation methods, and cover transport/ID/error differences. Keep denied methods explicit. Exit: tests prove forwarding and rejection against actual registered Go APIs; the existing fixture contract remains reproducible.
3. **C03 Chain corpus and strict comparator.** Build the state/fixture recorder, deterministic replay, error-aware comparison and reorg scenarios. Exit: inject wrong balances, dropped/duplicated logs, altered error wording, incorrect IDs, stale heads and one-bit transaction changes; every seeded defect is detected.
4. **C04 Client acceptance and dashboard.** Run the Quais.js versions and Pelagus read/preparation flows; produce method/case coverage with failures and exclusions. Exit: all v0.1 requirements pass, with no allowlist entry counted as coverage by itself.

Then start M2 while profiling which M3 native read path is worth implementing. The first engineering deliverable is a trustworthy compatibility report and a usable gateway; larger Rust rewrites are selected from measured costs.

## Source map

- [Go RPC registration](../go-quai/internal/quaiapi/backend.go) and [filters/local services](../go-quai/quai/backend.go).
- [Go callback naming](../go-quai/rpc/service.go) and [block selectors](../go-quai/rpc/types.go).
- [Quai API and Qi balance/range rules](../go-quai/internal/quaiapi/quai_api.go), [Ethereum API and transaction methods](../go-quai/internal/quaiapi/api.go), [filter queries](../go-quai/quai/filters/filter.go).
- [Quais.js request mapping and error handling](../quais.js/src/providers/provider-jsonrpc.ts), [socket subscriptions](../quais.js/src/providers/provider-socket.ts), [Pelagus dependency](../pelagus-extension/background/package.json).
- [Transaction encoding](../go-quai/core/types/transaction.go), [custom opcodes](../go-quai/core/vm/opcodes.go), [fork constants](../go-quai/params/protocol_params.go), [state processing](../go-quai/core/state_processor.go).
- [JSON-RPC 2.0 specification](https://www.jsonrpc.org/specification). It defines the envelope/batch rules; application and consensus compatibility come from the pinned Quai implementation and client tests.
