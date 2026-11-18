# Native Rust Quai client plan

Same text as in [NATIVE_STATEFUL_PROGRESS.md](NATIVE_STATEFUL_PROGRESS.md); omitted here.



The product target is a standalone Quai implementation whose shipped binaries,
chain authority, state storage, execution, indexing, networking and RPC are Rust.
Go is an offline compatibility oracle. It is excluded from the default build,
distribution and runtime. The old sidecar work is preserved as an isolated test
profile and in [the historical plan](../../docs/HYBRID_PLAN.md).

The current artifact is a Rust frozen-view RPC server and a bounded native
protocol checker with native transaction primitives and bounded stateful
execution/private transport libraries. It is not yet a validating or
synchronizing node. [Current weak-share/PoEM results](NATIVE_WEAKSHARES_RESULTS.md) and
[the active critical path](NATIVE_CONSENSUS_PROGRESS.md) record the latest scope.
[The performance ledger](../../PERFORMANCE_LEDGER.md) preserves source-bound gains and
regressions. Removing
Go probes does not replace their chain-validation responsibility; native block
validation and state transitions must establish that authority.

The [whole-source map](../../PORTING_MAP.md) inventories all tracked Go files and
schemas. [Network readiness](../../NETWORK_READINESS.md) defines the detailed G0–G11
promotion gates and private-network-to-public-canary path. The map is source
accounting, not a percentage ported or a Go test-coverage report.

## Milestones and gates

| Milestone | Work | Completion evidence | Current status |
|---|---|---|---|
| R0 Rust-only build boundary | Default Cargo features omit gateway/client; separate offline reference lab; native-only distribution | Build with Go denied and no Go checkout; reject Go-backed modes; SDK/frozen reads without an upstream | Implemented |
| R1 Protocol primitives | Exact Quai/Qi/ETX encoding, routing-bit transaction IDs, signing/recovery, block/body/receipt commitments, supported header and auxiliary-PoW forms | Frozen byte/hash vectors, malformed inputs and independent invariants, then cross-client wire cases | All raw schemas plus bounded canonical work-object/body/receipt/list commitments and transaction signatures implemented; typed-RLP decoding, complete auxiliary/resource domains and full validity remain open |
| R2 Consensus and execution | Header rules, all declared fork contexts, PoEM/chain selection, workshare and proof-of-work validation; serial Quai EVM and Qi/ETX state transitions | Full-block accepted/rejected replay from identical pre-state: all state/UTXO/ETX roots, receipts/logs, gas, fees and emitted ETXs agree | Nested execution, signed Quai/Qi/incoming settlement, lockup/authority/historical components and atomic zone composition implemented within tested bounds. Sixteen early whole Process blocks with native work, rewards/incoming coinbases and complete local image restores pass; 486 fee/reward projections cover selected forks. A separate early local-zone profile composes native header/body/manifest/PCRC with execution and rejects 31 valid-work mutations. A separately pinned early mixed-order profile adds prime/region/zone append, confirmed rollups, derived incoming coinbases and durable context witnesses. Mixed/nonlocal/fork/workshare/expansion histories and sealed consensus admission remain open |
| R3 Durable native chain state | Canonical chain/state/index transactions, snapshots, rollback journals and pruning | Crash at every commit boundary, competing branches, restart and deep reorgs; no partial head/index publication | Private scratch checkpoints, atomic fsync/head publication, reorg planning, corruption rejection and process-exit recovery implemented. Complete trusted local images now preserve account/ETX/raw-lockup/UTXO/MuHash state; private branch commits/recovery now include images, typed receipts and execution history with two actual-Go branches and process-exit tests. Early private native admission, frontier-selected canonical generations and crash recovery now pass. Production/global canonical state remains open |
| R4 Native P2P and sync | Quai libp2p negotiation, request streams, gossip, ancestor discovery, body/state acquisition, peer scoring and bounded queues | Independent controlled-network sync; bad peers/blocks rejected; restart and fork-choice match declared protocol | Optional enrolled loopback Rust libp2p Noise/Yamux, identify/Kademlia, manual gossip validation and early execution-checked block delivery implemented; fixed early private raw acquisition/canonical reorg/restart checks now pass across three stores in one process; full global/production native sync open |
| R5 Full RPC and wallet compatibility | Native account/UTXO reads, calls, estimates, access lists, logs/filters, subscriptions, externally signed submission and tx pool | Both pinned Quais.js variants and Pelagus flows on the Rust-owned test chain, including errors, reconnects and reorgs | Frozen-view subset implemented; live state/wallet/event behavior remains open |
| R6 Performance qualification | Profile complete workloads; optimize storage, copies, scheduling and validated parallel execution | Same outputs under fixed workloads; throughput/CPU/RSS/p99 and ingestion cost on representative Linux traces and sustained tests | Fixed source-bound state/signed-transaction, repeated proof-work and journal head-switch timing/separate allocation measurements recorded in the performance ledger; handler/allocation/HTTP profilers exist. Fresh work, disk, full replay, sustained RSS and full-client/network qualification remain open |

R1–R4 are required before replacing a validating Go node. Full RPC compatibility
also needs R5. Milestones can have independent test fixtures, but a passing
subset must not be reported as a completed full-client gate.

## Initial native protocol slice (historical)

`quai-primitives` implements a bounded Protobuf work-header codec, canonical
re-encoding, legacy/transition header commitments, activated seal hashing, and
exact Blake3 target arithmetic. BLAKE3 uses its pure Rust implementation.
Checked-in vectors are generated once from go-quai
official v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072`; ordinary Rust builds/tests consume them
offline. They cover fork boundaries, unknown fields, integer normalization,
missing mandatory fields and threshold arithmetic.

The scope is explicit: valid magnitudes up to 256 bits, prime heights up to u64,
location lengths 0–2, bounded data, and no active auxiliary header forms. Legacy
header commitments are not ProgPoW mining verification. Blake3 target tests are
not verification of KAWPOW, Scrypt or SHA donor chains. Bounded body/receipt/trie commitment checks now exist; complete proof, state and
hierarchy validation is still open. These helpers are not the RPC import trust gate.

## Compatibility contract

Pin the Go source revision, tracked diff, network/genesis, prime/region/zone
location, fork schedule and relevant limits. Do not treat a generic Ethereum
implementation as Quai compatible: Quai has custom opcodes (`ETX`,
`ISADDRINTERNAL`, `CONVERT`), access-list checks, address/ledger rules, Qi UTXOs,
conversions, lockups, rewards and cross-zone ETX effects. Transaction IDs are
not generic Ethereum RLP hashes: inspect the protobuf-derived hashing and
routing-bit rules in `core/types/transaction.go`.

Full replay starts from a declared pre-state and compares accept/reject, all
consensus commitments, account and UTXO state, receipt/log order and
multiplicity, gas, fees and ETX effects. Include before/at/after each relevant
fork with the correct height context. Keep errors and rejected blocks in the
corpus; do not normalize away mismatches or use Go fallback in the product.

The oracle only generates fixtures or runs in `reference-build/` as an explicit
development task. It never answers production RPC, owns production canonical
state, or runs inside the standalone distribution. Existing reference scripts
can run unchanged from that isolated source copy. Historical benchmark outputs
and their recorded contracts remain intact.

## Rust-owned authority and performance

The native validator commits the canonical head, account/UTXO state, receipt
index and log index as one coherent generation. RPC pins that generation for a
request. Index gaps are visible as unavailable; stale or unverified data cannot
be advertised as validated latest state. Native P2P supplies candidates, while
native consensus/execution decides what is canonical. No per-read Go probes or
fallback are part of that design.

Start execution serially. Parallel work requires isolated state, conflict
validation and deterministic merge, including Qi double-spend conflicts and
ETX/conversion/reward paths. Account access lists alone are not a complete
storage dependency graph. Move to parallel execution only after full-block
replay equivalence passes.

Optimization proceeds under a fixed correctness corpus and benchmark contract.
Measure complete block replay, state reads/writes, ingestion/rollback, realistic
RPC mixes and P2P queues separately. Keep expected outputs and fork rules outside
any tuner's control. The old native-read experiment's slowdown came from its
Go validation round trips; removing them is an architectural change, not a
measured claim about standalone consensus or execution speed.

## Native transaction increment

The native crate reconstructs bounded Quai/Qi/ETX Protobuf messages, canonical
bytes and routed IDs; computes reference signing messages; recovers Quai ECDSA
senders; verifies Qi Schnorr signature mathematics with unsorted, untweaked
MuSig2 input-key aggregation; and encodes reference typed RLP. There are 288
codec cases and 40 rejected inputs from the pinned offline oracle. These are
primitive checks, not accepted blocks or UTXO/state validation.
See [current results](NATIVE_ROUND_RESULTS.md).

The bounded domain is one MiB input Protobuf, 256-bit magnitudes, 128 KiB data
and 4,096 input/output/access/storage items. IDs use reconstructed Protobuf,
never generic Ethereum RLP hashes. Explicit origin routing is not
authentication. ETX signing data is empty in the reference. Qi typed RLP omits
work fields and is not a lossless replacement for transaction Protobuf.

## Native foundation increment (2026-11-09)

`quai-primitives` now includes every raw schema declaration and bounded
work-object/body/receipt/trie commitments. `quai-execution` adds intrinsic gas,
Qi input helper/unique-spend checks and live StateDB ETX queue/counter/root
semantics in memory. `quai-network` adds application frames/envelopes, request
correlation and topics. A raw TCP loopback test exchanges messages using the
pinned Go common stream helpers; it does not perform libp2p negotiation.
[Current evidence](NATIVE_FOUNDATION_RESULTS.md) records exact scope and limits.

## Next implementation batch

1. Qualify the intended network/genesis/fork/engine/fee profile (G0). The
   offline source baseline is official v0.57.0, including Polaris. The five
   prior local fee commits are archived and inactive. An exact source pin
   does not qualify a live network configuration.
2. Extend canonical semantic reconstruction to active auxiliary header/view
   forms and qualify full protocol limits; raw schema availability does not
   close G1. Finish typed-RLP ingress and commitment/state mismatch corpora.
3. Extend the native isolated VM/account layer through nested calls, creation,
   precompiles and full transaction settlement. Complete Qi
   output/value/fee/fork rules, conversions/rewards and inbound ETX eligibility.
   Build consensus proof/hierarchy/fork-choice differentials and durable state.
4. Plug the native application codec into a pinned Rust libp2p transport and
   test Noise/multiplexing, discovery and GossipSub on an isolated network.
   Then close independent validated private sync, public shadow validation and
   live RPC/pool/wallet acceptance before a production canary.
5. Keep performance experiments separate from compatibility development.
   The [allocation plan](../../docs/ALLOCATION_PLAN.md) remains proposed work; this
   increment makes no new throughput or whole-node memory claim.

## Serial VM increment (2026-11-09)

The execution crate now runs bounded isolated frames with arithmetic/memory,
control flow, state-scaled SLOAD/SSTORE gas and access-list checks, transient
storage/new-op fork gates, logs/revert and ISADDRINTERNAL. Five-field account
and secure storage/account roots match the frozen oracle. All 135 declared
supported opcode values appear in matched instruction traces.
[Evidence and exclusions](NATIVE_EVM_RESULTS.md) define the contract.

The next execution slice is a nested-frame state journal and call/create
lifecycle, followed by precompiles and transaction settlement. Original
storage, refund, access-list and transient state must survive/revert at the
correct transaction/frame boundaries. Then combine Quai, Qi and inbound ETX
execution into accepted/rejected full-block replay. Full consensus, durable
state and native libp2p sync remain independent open work packages.

## Consensus increment (2026-11-09)

The new `quai-consensus` crate implements full-size ProgPoW/KAWPOW light
kernels, SHA256d/scrypt donor hashes, ordinary difficulty adjustment, PoW-ID
fork rules and bounded PoEM order/rank/entropy projections. Supported
non-auxiliary headers bind to recomputed proofs and body commitments. Full
block admission remains explicitly unavailable.

Next: auxiliary canonicalization, template/quorum signatures, coinbase and
donor Merkle commitments; complete header/genesis/expansion policy; uncle/
workshare history and pre-fork discounts; hierarchy/termini branch selection.
Integrate economic state replay and durable canonical publication before
independent network validation. [Evidence](NATIVE_CONSENSUS_RESULTS.md).

## Signed auxiliary increment (2026-11-09)

Bounded explicit auxiliary-aware codecs now reconstruct AuxPow/template
semantics and header hashes. Native quorum checks try all six ordered 2-of-3
pairs with the pinned public keys; BTC/BCH version masking and coinbase/
donor Merkle/Quai seal/time and Litecoin/Dogecoin commitments match the oracle.
Valid supported KAWPOW block headers continue through the native light engine.
[Evidence](NATIVE_AUXILIARY_RESULTS.md) distinguishes these components from
full block/share admission. The default legacy codec keeps its previous
unsupported-domain boundary; explicit auxiliary-aware methods extend the
contract without changing frozen baseline expectations.

Next: header/uncle validation against ancestry, workshare eligibility/targets/
counts/distance and pre-fork discounts, followed by genesis/expansion and
hierarchy fork-choice policy. Full economic state replay and durable canonical
publication are still required before independent network validation.

Candidate ancestry and share-controller increment (2026-11-09): bounded native history/inclusion components and pinned-source differential evidence are tracked in `NATIVE_HISTORY_RESULTS.md`. Next: qualify full header entropy/base-fee/expansion/hierarchy policy, share rewards, competing branches and durable replay before canonical publication.

Zone-header increment (2026-11-09): bounded zone `verifyHeader` policy, conversion/base-fee arithmetic and composed native proof/body inspection are tracked in `NATIVE_HEADER_RESULTS.md`. Next: complete prime/region header policy and genesis/expansion initialization, reward distribution/state replay and competing-branch fork choice.

Prime/region increment (2026-11-09): header policy, efficiency/threshold/ETX controllers and native component assembly are tracked in `NATIVE_HIERARCHY_RESULTS.md`. Next: complete hierarchy placement/termini/interlink eligibility and expansion/genesis initialization, reward/state replay and competing-branch fork choice.


Candidate append increment (2026-11-10): native order/rank/routing, append
manifest/interlink commitments and PCRC preparation are tracked in
`NATIVE_PLACEMENT_RESULTS.md`. Next: expansion/genesis initialization and
multi-block competing-branch fixtures, then economic/state replay, fork choice
and atomic durable canonical publication. Candidate index entries remain
unverified; successful preparation does not admit a block.


Genesis/candidate-branch increment (2026-11-10): bounded canonical genesis
construction, expansion geometry, explicit default-genesis difficulty and
atomic candidate storage now have source and native multi-block evidence in
[NATIVE_GENESIS_RESULTS.md](NATIVE_GENESIS_RESULTS.md). Two sibling branches
advance without canonical selection. Next: qualify expansion allocation/state
initialization and cross-context branch scoring/eligibility, then economic replay,
fork choice and durable reorg/crash recovery. Full block admission and sync
remain unavailable.


PoEM/frontier increment (2026-11-10): native local candidate scoring, constructed
expanded-genesis zone startup, coordinator NodeSet projections and entropy-keyed
pending views are tracked in [frontier results](NATIVE_FRONTIER_RESULTS.md).
Go's legacy location names alias regions >=3; default views reject ambiguous
layouts and an explicit consistent legacy view reproduces that source behavior.
Runtime coordinator/generation integration, allocations and economic replay,
canonical fork choice, durable state/reorg recovery and native sync remain open.


Global-eligibility increment (2026-11-10): bounded imported multi-context ancestry,
constraint tracing and cross-context PCRC frontier gates now compose with native
candidate scoring. [Eligibility results](NATIVE_ELIGIBILITY_RESULTS.md) record
14,328 comparisons, including six native coincident blocks checked in all three
contexts. Consistency checks preserve unverified-history status. Next: runtime coordinator/generation integration and candidate retention, followed
by allocation/economic replay and durable canonical state before network sync.


Coordinator selection/reapplication increment (2026-11-10): recent admission,
exact pinned-Go tie ordering, bounded global leader search/retries and native
score-bound pending-frontier reapplication are tracked in
[coordinator results](NATIVE_COORDINATOR_RESULTS.md). The new corpus has 42,731
comparisons; all outputs retain unverified-history status. Generation workers,
pending-header body assembly and runtime event integration remain open. Next:
define block pre-state/results, complete nested-call and transaction/Qi/ETX
economic replay, and add durable canonical commits/reorg recovery before sync.


Official upstream update (2026-11-10): the active source baseline is **v0.57.0**
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`, including the merged Polaris changes.
Local fee-feature commits are preserved separately on
`backup/pre-polaris-fees-20261110`. [Migration results](NATIVE_UPSTREAM_RESULTS.md)
record binding/identity/Qi-input changes, fresh corpora and the economic/network
work that remains open. Polaris activates at Prime terminus block 2,444,574.
The current runtime remains a frozen unverified RPC view with native libraries;
full transaction replay, canonical state and native sync remain unavailable.
