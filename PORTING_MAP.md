# Go-to-Rust coverage map

Audited 2026-11-16 against go-quai 33db82f2891b267ba898ca38bd2dd8bf75cbb072 (VERSION v0.57.0).
This accounts for all 506 tracked Go files: 349
non-test and 157 test files, plus 5 Protobuf schemas.
Non-test includes generated files, mocks and support helpers. Every file has
one primary owner in the machine map; components can have shared references.
Dependency source outside this checkout is not inventoried. The pinned go.mod/
go.sum hashes identify that separate compatibility audit surface.

**This is not a completion percentage or line/function test-coverage report.**
File counts do not establish behavioral equivalence. No entire Go subsystem
is marked fully ported. The RPC executable continues to serve frozen imported reads. Libraries now add
bounded nested execution, signed Quai/Qi settlement, UTXO/MuHash commitments,
16 linked early private whole-block execution differentials with reward payouts
and complete local state images, durable scratch checkpoints and optional private libp2p transport. Full block consensus, complete
historical economic state and global canonical native sync remain open. The fixed early private profile now qualifies native canonical publication and loopback raw-block acquisition; see NATIVE_CANONICAL_RESULTS.md.
[Stateful results](NATIVE_STATEFUL_RESULTS.md) record the exact scope.

The published v0.57.0 release matches the pinned official source
reference, verified against the release page on 2026-11-13. The five local
fee-feature commits are preserved separately and are inactive in this profile.
Polaris activates at Prime terminus block 2,444,574. A network target must
choose and hash the exact fork/configuration profile, rather than rely on the
VERSION label. [Official release](https://github.com/dominant-strategies/go-quai/releases/tag/v0.57.0).

| Primary owner | Native scope status | Go non-test / test files | Native scope actually implemented |
| --- | --- | ---: | --- |
| [Genesis, chain configuration and fork contexts](../go-quai/params/config.go) | partial | 8 / 2 | Official allocation data imported into a native height-indexed asset; 28 allocation-root comparisons include Singularity and Polaris forfeiture changes. Private chain 1337 source/config/genesis remain pinned. Public target and complete initialization qualification remain open. |
| [Common and core Protobuf schema](../go-quai/common/proto_common.proto) | partial | 2 / 0 | All 67 raw message declarations/codecs across five schemas; bounded preflight and canonical body/header/transaction reconstruction. Bounded ProtoTermini reconstruction enforces 16 dominant and 16 subordinate hashes and canonicalizes supported short hashes. |
| [Quai/Qi/ETX codecs, IDs and signature mathematics](../go-quai/core/types/transaction.go) | partial | 16 / 8 | Canonical transaction Protobuf, typed-RLP encoder, routed IDs, signing messages, ECDSA recovery and Qi MuSig2/BIP340 mathematics. |
| [Full headers, work objects and body commitments](../go-quai/core/types/wo.go) | partial | 4 / 4 | Bounded canonical body headers/complete work objects, body-header binding and zone/region/prime body commitments. |
| [Receipts, logs and consensus commitments](../go-quai/core/types/receipt.go) | partial | 5 / 0 | Native consensus receipt/log RLP, 10,240-byte blooms and ordered receipt roots. |
| [Auxiliary PoW, donor formats and quorum templates](../go-quai/core/types/auxpow.go) | partial | 7 / 4 | Bounded auxiliary/template codecs and hashes; pinned 2-of-3 MuSig2 verification; coinbase/Quai seal/time/donor Merkle and Litecoin/Dogecoin commitments; signed KAWPOW block proof integration. Full block/share history and economic admission remain open. |
| [PoW engines and complete header rules](../go-quai/consensus/consensus.go) | partial | 14 / 7 | Native ProgPoW/KAWPOW proof kernels, supported signed auxiliary binding and donor hashes, bounded header/share/entropy controllers and hierarchy components. The early private-hierarchy profile now composes native work, all required context views, body/placement, strong/weak pre-fork shares, confirmation and state roots into a store-bound private admission permit. Native zone entropy selects a three-tip frontier and atomically publishes the selected private checkpoint. Twenty-five actual-Go coordinator/topology events qualify 24 native blocks, a higher-entropy shorter-branch reorg and recovery. This is not full global block admission: later forks/auxiliary economics, expansion, nonlocal histories, mixed transaction blocks and complete coordinator generation/leader runtime remain open. |
| [PoEM, hierarchy, fork choice and expansion](../go-quai/core/poem.go) | partial | 5 / 2 | Bounded native order/rank/entropy, context headers, manifests/interlinks/PCRC, genesis geometry, multi-context ancestry/eligibility and Go coordinator search/reapplication components. A fixed early single-zone private profile now has sealed admission, native frontier selection, immutable canonical generations and crash recovery, matched to 25 actual Go BuildPendingHeaders/SetCurrentHeader events. Full global coordinator worker generation, multi-zone and nonlocal fork selection remain unqualified. The hash-only scoring projection still refuses nonempty pre-fork shares; the composed private path uses authenticated native entropy instead. Activated local components now compose native header/body/manifest/PCRC, authenticated cross-prime rollups, and signed Qi/Quai/standard-ETX execution from an explicit synthetic height-259200 checkpoint. Four sealed accepted blocks and 33 valid-work rejections are conditional component evidence, not added genesis-to-head canonical coverage. The existing canonical path now executes from an exact-block/profile-bound hierarchy preparation instead of a cloned inbound list. Full mixed canonical and later/nonlocal/fork coverage remain open. All seven currently defined incoming ETX kinds now compose with signed Quai/Qi and hierarchy append/PCRC in conditional supplied-checkpoint tests. This grants no canonical ancestry or runtime widening. Exact native prime-controller arithmetic now matches 3200 actual Go comparisons, including signed floor division, resets/holds/freeze and the existing miner EWMA. Authenticated window/history and complete admission integration remain open. A branch-bound native 4000-entry token-choice window now matches 4216 actual Go updates, including signed correction rounding, rolling replacement, atomic persistence and fork/recovery checks. Complete controller history/admission integration remains open. |
| [Serial block processing and transition orchestration](../go-quai/core/state_processor.go) | partial | 7 / 13 | Atomic serial zone composition of signed Quai/Qi/incoming settlement, fee ordering/feedback, reward-depth payouts, historical effects and execution commitments. Qualified on 16 linked early private actual Process blocks with real native proofs and restored local images. Full mixed/fork/hierarchy admission remains open. Execution-checked private candidates now stage complete durable branch commits, with 24 unique blocks across two actual-Go branches and explicit selection/recovery. A separately pinned pre-transaction local-zone profile now enforces native header/body/manifest/PCRC components before atomic execution and private staging; 31 real-work field mutations are rejected without publication. A separately pinned early profile now composes native prime/region/zone append, authenticated rollup commitments, ordered confirmed ETX derivation and incoming coinbase execution with durable branch witnesses. Two local branches qualify 24 unique blocks and 38 context bundles; mixed/nonlocal/fork/workshare/expansion and sealed global admission remain open. Pre-KAWPOW full-target ProgPoW uncles now compose integer distance-discounted entropy, parent PoEM fields, payouts/confirmation and incoming coinbase execution in that early profile. The earlier Go-valid weak-share capability case is now accepted; native binary-precision entropy and supplied-ancestry candidate scoring have bounded private qualification. Fresh parent state is restored once within each hierarchy append, with current-call ancestry checksums and a separate stage-parent check. Exact weak-share rounding has 5,875 controlled Go arithmetic cases; native three-context branches add 760 comparisons, 10 weak shares, 13 full-target inclusions, 30 incoming coinbases and 15 local candidate scores. Current native score components still grant no verified-history/canonical authority. Nonlocal/mixed/auxiliary/fork/expansion and sealed full admission remain open. Activated local components now compose native header/body/manifest/PCRC, authenticated cross-prime rollups, and signed Qi/Quai/standard-ETX execution from an explicit synthetic height-259200 checkpoint. Four sealed accepted blocks and 33 valid-work rejections are conditional component evidence, not added genesis-to-head canonical coverage. The existing canonical path now executes from an exact-block/profile-bound hierarchy preparation instead of a cloned inbound list. Full mixed canonical and later/nonlocal/fork coverage remain open. All seven currently defined incoming ETX kinds now compose with signed Quai/Qi and hierarchy append/PCRC in conditional supplied-checkpoint tests. This grants no canonical ancestry or runtime widening. |
| [Quai EVM, custom opcodes and precompiles](../go-quai/core/vm/evm.go) | partial | 24 / 8 | Bounded nested CALL/CREATE, precompiles, custom opcodes and native lockup calls, including raw-batch rollback forks. Expanded actual-Go corpus covers 1548 VM cases/20124 comparisons. Full historical/economic domains remain open. Account and contract slot access now uses fallible pinned providers and bounded write overlays; the journal captures/finalizes touched size baselines and backend unavailability aborts nested CALL/CREATE and settlement. Full global EVM/resource qualification remains open. |
| [Qi/UTXO/ETX state rules and journals](../go-quai/core/state_processor.go) | partial | 3 / 1 | Atomic Qi settlement plus native incoming dispatch for all seven ETX types, ordered queues, coinbase/lockups, conversions/wrapping and historical trimming components. Independent committed size/MuHash preserves historical duplicate-delete/overwrite behavior. Full block replay, eligibility and persistence remain open. Complete local state images preserve pending queue metadata and independent MuHash/count, including legacy raw-map ghosts; durable canonical publication remains open. A separately pinned early profile now composes native prime/region/zone append, authenticated rollup commitments, ordered confirmed ETX derivation and incoming coinbase execution with durable branch witnesses. Two local branches qualify 24 unique blocks and 38 context bundles; mixed/nonlocal/fork/workshare/expansion and sealed global admission remain open. Pre-KAWPOW full-target ProgPoW uncles now compose integer distance-discounted entropy, parent PoEM fields, payouts/confirmation and incoming coinbase execution in that early profile. The earlier Go-valid weak-share capability case is now accepted; native binary-precision entropy and supplied-ancestry candidate scoring have bounded private qualification. Fresh parent state is restored once within each hierarchy append, with current-call ancestry checksums and a separate stage-parent check. Exact weak-share rounding has 5,875 controlled Go arithmetic cases; native three-context branches add 760 comparisons, 10 weak shares, 13 full-target inclusions, 30 incoming coinbases and 15 local candidate scores. Current native score components still grant no verified-history/canonical authority. Nonlocal/mixed/auxiliary/fork/expansion and sealed full admission remain open. All seven currently defined incoming ETX kinds now compose with signed Quai/Qi and hierarchy append/PCRC in conditional supplied-checkpoint tests. This grants no canonical ancestry or runtime widening. |
| [Rewards, fees, conversion and controller math](../go-quai/consensus/misc/rewards.go) | partial | 4 / 4 | Bounded reward/conversion/base-fee math, incoming economic dispatch, coinbase interest, authority updates, indexed allocations, redemption and trimming. Exact official fee feedback/reward-depth branch has 486 projection cases; 16 early private whole blocks qualify native payouts and incoming coinbases. Complete historical economic replay remains open. Exact native prime-controller arithmetic now matches 3200 actual Go comparisons, including signed floor division, resets/holds/freeze and the existing miner EWMA. Authenticated window/history and complete admission integration remain open. A branch-bound native 4000-entry token-choice window now matches 4216 actual Go updates, including signed correction rounding, rolling replacement, atomic persistence and fork/recovery checks. Complete controller history/admission integration remains open. |
| [Local fee-fallback feature-branch behavior](../go-quai/params/config.go) | missing | 0 / 0 | Local fee-reserve extensions are preserved on backup/pre-polaris-fees-20261110 at db42af74d31f25390236ffeca14cca2cba7de639. They are not part of the official v0.57.0 reference profile and are not enabled in the native runtime. |
| [Account/storage state, journaling and snapshots](../go-quai/core/state/statedb.go) | partial | 25 / 1 | Copy-on-write snapshots and invalidation-aware commitment caches, block-deferred storage sizing and raw-lockup rollback. Complete trusted local images preserve accounts/storage, pending ETX indices/metadata, raw lockups and legacy UTXO/MuHash/count state; restored after each of 16 private blocks. Persistent canonical branch state and crash recovery remain open. Complete state images now persist with receipts and history in the private branch journal, qualified on two actual-Go branches; production canonical state remains open. World account-state commitments now copy only changed secure-trie paths and preserve prior snapshot roots. Aggregate account/storage/byte quota validation still applies; account maps and per-account storage remain bounded in memory. Native private canonical execution now restores account/storage/code from keyed database versions and persists changed records. Signed Quai, Qi and incoming-ETX components retain frozen Go outputs after persisted before/after state roundtrips. World maps are still bounded and materialized; lazy production access remains open. |
| [Trie and state/UTXO commitments](../go-quai/trie/trie.go) | partial | 15 / 10 | Pure Rust ordered/raw/secure-key MPT commitments, immutable path-copying secure trie updates/deletes, hash-addressed lazy node reads, exact UTXOHash and MuHash3072. Incremental World account roots reuse unchanged branches. A transactional native database persists trie nodes and immutable fork roots; actual-Go qualification covers 70,000 keys and 5,000 state-version commits. The native private canonical path now uses the keyed database; lazy/global execution providers and production storage qualification remain open. |
| [Durable canonical chain/state database](../go-quai/core/rawdb/schema.go) | partial | 22 / 5 | The fixed private canonical writer loads account/code-for-requested-accounts, contract storage, ETX queue/economic metadata, raw lockups and Qi membership on demand from pinned snapshots. Touched records, root sets, independent MuHash/count, version metadata and indexes commit together; indexed admission/frontier records and selected-version CAS govern native private recovery/reorg. Format-5/codec-2 storage is unchanged from the prior sparse-account source, with old/current binary reopen checked at the small frozen profile. Overlay/cache/export limits remain. Production scale, complete admission, indexes/pruning and platform faults remain open. |
| [Bloom, address/outpoint and RPC indexes](../go-quai/core/bloom_indexer.go) | partial | 7 / 1 | Rust indexes imported frozen receipts/logs only. |
| [Transaction admission, pool and persistence](../go-quai/core/tx_pool.go) | missing | 5 / 0 | Submission is disabled in the shipping RPC service. |
| [Peer transport, identity and resource management](../go-quai/p2p/node/node.go) | partial | 9 / 2 | Optional Rust libp2p 0.56.0 Noise/Yamux persistent-stream transport with enrollment, IPv4-loopback filtering before TCP, stream/connection/frame limits and poisoned streams after I/O failure. Eight actual Go/Rust request pairs and three early Rust/Rust block deliveries passed; default RPC binaries do not enable transport. |
| [Peer discovery, scoring and peer database](../go-quai/common/bootnodes.go) | partial | 7 / 5 | Optional private identify and Kademlia /quai behavior; only enrolled loopback addresses are admitted to the routing table. Persistent identities/peer databases, bootstrap/liveness/scoring and network discovery qualification remain open. |
| [Quai streams, request/response envelopes and framing](../go-quai/common/stream_services.go) | partial | 13 / 9 | Native bounded Quai framing/envelopes and ID/location/type correlation plus optional enrolled loopback Noise/Yamux persistent streams. Existing real Go/Rust interoperability evidence is retained. Private raw block acquisition now checks response hashes, strict parent heights, required context witnesses and bounded bytes/block count/deadline; semantic failures close the stream. Global peer scheduling and production transport/resource qualification remain open. |
| [Gossip topics, workshares, blocks and auxiliary templates](../go-quai/p2p/node/pubsubManager/gossipsub.go) | partial | 2 / 4 | Optional signed strict GossipSub with manual validation before forwarding. Three-peer test proves messages wait for acceptance and rejected payloads are withheld. Full chain-data admission and production scoring/resource qualification remain open. |
| [Native chain acquisition and synchronization](../go-quai/quai/handler.go) | partial | 4 / 3 | Reusable private_sync and an optional private-node runner acquire raw zone/context objects over actual loopback Noise/Yamux, verify ID/location/type/hash and native work/order/ancestry, then import through sealed native private admission. Independent Rust OS processes qualify the early profile, one native fork reorg, receiver/source restarts, SIGKILL recovery and cancellation with exact Go roots/head. Persistent checksummed peer identities retain enrollment across restarts. Known heads need no peer connection. Broader forks, mixed transactions, global coordinator, production peer scheduling and automatic head following remain open. |
| [JSON-RPC transport, IDs, errors and limits](../go-quai/rpc/server.go) | partial | 15 / 5 | Native HTTP JSON-RPC subset with IDs, notifications, batches, limits and backpressure. |
| [Node RPC methods and filters](../go-quai/internal/quaiapi/backend.go) | partial | 11 / 6 | Eight frozen-view read methods; live state/calls/estimates/submission are unavailable. The official v0.57.0 Polaris update is now the source baseline; implementation scope and remaining gaps are recorded in NATIVE_UPSTREAM_RESULTS.md. |
| [Client/ABI libraries and wallet integration](../go-quai/interfaces.go) | deferred | 15 / 9 | Pinned Quais.js read acceptance exists; no Rust client/ABI library or complete wallet flow. |
| [Node daemon, CLI, config and lifecycle](../go-quai/node/node.go) | partial | 23 / 8 | Default binaries remain a frozen-view read-only RPC service and bounded protocol checker. An optional separately packaged private-node runner composes early-profile native admission/selected state with actual peer acquisition in independent OS processes. It has persistent checksummed peer identity, bounded local JSON control, one acquisition/import worker, cancellation between atomic block commits and clean shutdown. It requires an explicit source address and announced head. A complete production validating daemon, global protocol replay, public target and live state RPC remain open. |
| [Block/workshare construction, mining RPC and Stratum](../go-quai/core/worker.go) | deferred | 7 / 5 | No native block producer or Stratum server. The official v0.57.0 Polaris update is now the source baseline; implementation scope and remaining gaps are recorded in NATIVE_UPSTREAM_RESULTS.md. |
| [Other cryptographic primitives and precompile backends](../go-quai/crypto/crypto.go) | partial | 16 / 6 | Native ECDSA recovery, SHA256/RIPEMD160/identity, modular exponentiation, BN254 add/mul/pairing and Blake2F precompile backends plus exact MuHash3072 hashing. Complete protocol/domain and whole-client parity remain open. |
| [Native event feeds and canonical notifications](../go-quai/event/feed.go) | missing | 4 / 7 | No committed-chain event/subscription system. |
| [Metrics, logs, statistics and operations](../go-quai/metrics_config/metrics.go) | partial | 9 / 1 | Basic frozen-service health counters and benchmark instrumentation. |
| [Build, release and offline compatibility tooling](../go-quai/build/ci.go) | support | 7 / 0 | Native Go-denied build/distribution and isolated offline Go oracle are implemented. |
| [Shared address/math/configuration helpers](../go-quai/common/address.go) | partial | 25 / 17 | Selected primitive helpers exist; reuse Rust libraries where behavior matches. |
| [Go mocks, fuzz/support infrastructure](../go-quai/p2p/mocks/mockedHost.go) | support | 9 / 0 | Rust unit/differential/SDK harnesses cover bounded implemented subsets. |

All 67 declared messages have raw native field codecs,
qualified by frozen schema vectors. The designated bounded work-header/transaction/auxiliary
slice covers 16 declarations; newer body/receipt/trie semantics are
tracked by component evidence. None of those counts is a completion percentage.
Bounded auxiliary semantic codecs and signed bindings now exist. Complete
execution/consensus and workshare/economic policy remain open. Optional private
libp2p transport exists; its existence does not qualify full native sync.

The current evidence is 281 Rust tests and
6,979 bounded protocol comparisons, plus
70 frozen RPC and 16 SDK cases.
There are also 0 native execution-helper/live-ETX-state
comparisons and 0 bounded VM/state-gas
comparisons. [VM results](NATIVE_EVM_RESULTS.md) and [foundation results](NATIVE_FOUNDATION_RESULTS.md)
distinguish their scopes. There are also 0
consensus kernel/rule comparisons; [consensus results](NATIVE_CONSENSUS_RESULTS.md)
list the remaining full-admission gates. Signed auxiliary verification adds
0 comparisons; [auxiliary results](NATIVE_AUXILIARY_RESULTS.md)
record those bounded domains. Candidate ancestry and inclusion add
0 comparisons; [history results](NATIVE_HISTORY_RESULTS.md)
separate controlled-hash policy from native signed KAWPOW cases. Imported history
remains unverified. Bounded zone header/conversion policy adds
0 comparisons; [header results](NATIVE_HEADER_RESULTS.md)
separate parent policy, candidate proofs and economic replay. Prime/region header
policy adds 0 comparisons;
[hierarchy results](NATIVE_HIERARCHY_RESULTS.md) record the wrapping/bitmap rules,
actual native proof fixtures and remaining canonical/hierarchy-placement gates.
Bounded candidate append preparation adds 0
comparisons; [placement results](NATIVE_PLACEMENT_RESULTS.md) distinguish computed
order/routing, local parent manifest/interlink commitments and PCRC candidate
transitions from canonical branch admission. Full placement qualification,
complete expansion/genesis state initialization, economic replay and fork choice remain open.
Canonical genesis/geometry/difficulty and multi-block candidate branch storage add
0 comparisons; [genesis results](NATIVE_GENESIS_RESULTS.md)
separate payload construction and in-memory branch isolation from allocation
replay, durable canonical state and fork choice. Expanded-genesis startup,
local PoEM scores and coordinator projections add 0
comparisons; [frontier results](NATIVE_FRONTIER_RESULTS.md) document explicit
legacy location-name aliasing and the open global coordinator/fork-choice gates.
Bounded multi-context ancestry, constraint tracing and PCRC frontier gates add
0 comparisons;
[eligibility results](NATIVE_ELIGIBILITY_RESULTS.md) separate six native coincident
blocks from controlled policy projections. Coordinator runtime integration,
economic replay and canonical fork choice remain open.
Recent admission, exact pinned-Go ordering, bounded leader search/retries and
pending-frontier reapplication add 0
comparisons; [coordinator results](NATIVE_COORDINATOR_RESULTS.md) distinguish
controlled projections from native-proof cases and open generation/runtime gates.
Polaris fork-boundary evidence adds 0
comparisons; [upstream migration results](NATIVE_UPSTREAM_RESULTS.md) distinguish
ported binding/identity/Qi-input checks from unimplemented economic and network
changes. Fee feedback/reward-depth arithmetic and 16 linked early whole-block
execution replays add 0 checks;
[NATIVE_CONSENSUS_RESULTS.md](NATIVE_CONSENSUS_RESULTS.md) distinguishes controlled
work projections, real native proof kernels and trusted local state-image
restoration from complete hierarchy/admission and canonical durability/sync.
Two private execution branches add 289
state/receipt/history recovery and branch-switch comparisons;
[durable results](NATIVE_DURABLE_RESULTS.md) separate explicit private selection
from PoEM canonical publication and production storage. A distinct local-zone
profile adds 284 real-Go matched
header/body/manifest/PCRC and execution checks, including valid-work mutations.
[Local admission results](NATIVE_ADMISSION_RESULTS.md) preserve its pre-transaction,
non-dominant boundary. A separate early mixed-order profile adds
430 comparisons for native
prime/region/zone append, committed rollups, derived incoming coinbases and durable
context witnesses. [Confirmation results](NATIVE_CONFIRMATION_RESULTS.md) retain
its original single-zone/no-workshare boundary. A further strong-uncle corpus adds
505 comparisons for full-target
ProgPoW inclusion, integer entropy, payouts and confirmed incoming coinbases.
[Uncle results](NATIVE_UNCLES_RESULTS.md) retain their original strong-only scope. The current weak-share increment adds
6,637 checks: controlled binary-precision
arithmetic plus native strong/weak share, PoEM, reward and branch execution.
[Weak-share results](NATIVE_WEAKSHARES_RESULTS.md) distinguish bounded score
components from full canonical fork choice and retain remaining fork/sync gates.
Conditional activated coverage adds 0
local blocks and 0 distinct
coincident zone blocks. The coincident suite has
0 checks and
0 valid-work mutations.
[Activated hierarchy results](NATIVE_COINCIDENT_RESULTS.md) separate supplied-checkpoint
execution and branch recovery from authenticated ancestry and canonical admission.
These blocks do not extend the 24-block genesis-linked early canonical corpus.

The experimental genesis-only runtime qualifies
8,192 pre-transaction blocks with
122,930 replay/reopen assertions and
8,192 separately acquired blocks across
64 bounded forward windows.
[Genesis/sync results](NATIVE_GENESIS_SYNC_RESULTS.md) preserve the separate runtime
fingerprint, exact-executable replay-evidence reuse, fresh process checks and unqualified
transaction-activation boundary. These figures do not promote the public network gates.

Repeated benchmark operations are not additional feature coverage.

See the [machine-readable map](compatibility/go-quai-porting-map.json) for
every tracked path/hash, declared schema message, native source/evidence link,
component gap and proposed crate. [Network gates](NETWORK_READINESS.md) define
the route to a node; [allocation work](docs/ALLOCATION_PLAN.md) separates memory
traffic improvements from retained RAM and network readiness.

Refresh or detect drift without contacting a network:

    python3 scripts/refresh_porting_map.py --go-quai ../go-quai --write
    python3 scripts/refresh_porting_map.py --go-quai ../go-quai --check
    python3 scripts/check_network_plan.py

The [incremental state database](NATIVE_STATE_DB_RESULTS.md) qualifies secure trie mutations, durable fork versions and pinned reads against actual Go. The [native private canonical path](NATIVE_CANONICAL_DB_RESULTS.md) now uses it for keyed execution state; bounded map materialization, ancestry/catalog limits, production indexes and global replay remain open.

The generator rejects unclassified paths, missing references, changed native
source versus build evidence and a different/dirty pinned Go baseline. Review
component assignments when adding files; generated accounting is not a
substitute for functional review. Port externally observable behavior using
Rust implementations/libraries; a literal translation of every Go helper or
database file format is not required.
