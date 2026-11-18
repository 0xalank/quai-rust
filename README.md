The latest qualified increment adds [linked prime-forward economics](NATIVE_PRIME_FORWARD_RESULTS.md), [optimized-trie and writer-lock stability](NATIVE_STORAGE_STABILITY_RESULTS.md), and a separately archived [259,216+16-block historical receiver](NATIVE_FOLLOWED_GENESIS_RESULTS.md). The installed source passes 281 release tests and 30 sequential qualification gates. Global full consensus and public sync remain unfinished.

# quai-rust

Current increment: [native prime-forward refunds, repricing and linked controller/token-choice updates](NATIVE_PRIME_FORWARD_RESULTS.md), qualified with 281 Rust tests. This is a supplied-history component path; full append admission, global consensus and public sync remain unfinished.

Current increment: [branch-bound native token-choice windows](NATIVE_CHOICE_WINDOW_RESULTS.md), with 4216 Go window updates and atomic recovery checks. Full consensus and public sync remain open.

Current increment: [native prime economic controllers](NATIVE_CONTROLLER_RESULTS.md), with 3200 Go arithmetic comparisons. Full consensus and public sync remain open.

Current increment: [composed execution for every defined ETX kind](NATIVE_ETX_FAMILY_RESULTS.md), with state/recovery and valid-work rejection checks. Full consensus and public sync remain open.

Current native increment: [full proof reuse for inclusion and rewards](NATIVE_WORK_REUSE_RESULTS.md) removes repeated native work calculations and qualifies the streaming-replay to independent-receiver handoff. Full global consensus and public sync remain unfinished.

Current sync increment: [local proof reuse](NATIVE_PROOF_REUSE_RESULTS.md) shares exact native header proofs and an immutable epoch cache across acquisition/admission. Full global consensus and public sync remain unfinished.

Current state loading: [sparse ETX, economic metadata and lockups](NATIVE_LAZY_AUX_RESULTS.md) completes on-demand record loading for the private canonical execution state. Existing format-5 encoding is preserved. Full global consensus and public sync remain unfinished.

Current execution state: [sparse accounts and contract storage](NATIVE_LAZY_WORLD_RESULTS.md) extends on-demand Qi/catalog/ancestry reads to account/code/slot access and touched-key persistence. New private disk formats require a fresh datadir; old formats are preserved and refused.

Current storage increment: [pinned execution-state lifetime](NATIVE_PINNED_STATE_RESULTS.md) preserves sparse reads across writer close/reopen while keeping one writer per file inode.

Current native runtime: [sparse Qi state](NATIVE_LAZY_QI_RESULTS.md) loads UTXOs on demand and persists changed membership keys; [indexed canonical storage](NATIVE_INDEXED_CATALOG_RESULTS.md) supplies native admission receipts/frontiers and on-demand ancestry recovery. Independent private-node processes qualify early-profile sync/reorg, persistent peer identity, restart/kill recovery, cancellation and clean shutdown. Full global consensus/public sync remain unfinished; the tracked goal is active.



A native Rust Quai client project. The default build and distribution use Rust
binaries with no Go toolchain, Go checkout, Go node, or upstream gateway.
Go remains an explicit offline compatibility oracle in a separate test profile.

The RPC executable serves a **frozen imported read-only view**. Native libraries
now add journaled nested execution, signed Quai/Qi settlement, UTXO/MuHash
commitments, private block replay with reward payouts/incoming coinbases,
complete state/receipt/history recovery across two private branches and
optional enrolled loopback libp2p transport. Full consensus, historical economic
replay and canonical network sync remain open. See the
[current independent-node results](NATIVE_PROCESS_RESULTS.md). The
[performance ledger](PERFORMANCE_LEDGER.md) retains measured gains, regressions,
raw samples and source hashes. The [native client plan](PLAN.md) records the remaining gates.
The [whole-source coverage map](PORTING_MAP.md) inventories every tracked Go
file/schema and the remaining native scope. [Network readiness](NETWORK_READINESS.md)
sets the private-sync, public-shadow and production-RPC promotion criteria.

A separate [local-zone admission profile](configs/private-admission-v1.json) now
checks native header, body, manifest/PCRC and execution components before private
staging. A separate [hierarchy profile](configs/private-hierarchy-v1.json) now adds
native prime/region/zone append, confirmed ETX rollups, derived incoming coinbase
execution and durable context witnesses. Full-target pre-fork uncles now add
native inclusion, entropy and reward qualification. Weak pre-fork shares now
add exact binary-precision arithmetic and native candidate scoring. General fork/workshare/mixed transaction
admission and canonical synchronization remain open.

The active compatibility baseline is official upstream **v0.57.0** source
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`. See the
[Polaris migration report](NATIVE_UPSTREAM_RESULTS.md) for fork rules, evidence
and remaining gaps. The earlier local fee overlay is archived separately.

## Build and test

```sh
python3 scripts/build.py
python3 scripts/verify_rust.py
python3 scripts/verify_vm.py
python3 scripts/verify_stateful.py
python3 scripts/verify_zone_execution.py
python3 scripts/verify_private_branches.py
python3 scripts/verify_admission.py
python3 scripts/verify_confirmation.py
python3 scripts/verify_uncles.py
python3 scripts/verify_weakshares.py
python3 scripts/verify_canonical.py --peer
python3 scripts/verify_private_processes.py
python3 scripts/build_private_node.py
python3 scripts/verify_consensus.py
python3 scripts/verify_auxiliary.py
./bin/quai-protocol-check fixtures/protocol/work-headers.json
./bin/quai-protocol-check fixtures/protocol/transactions.json
./bin/quai-protocol-check fixtures/protocol/foundation.json
```

Rust/Cargo are the implementation toolchain. Python runs the optional project
build/test helpers. The equivalent native build is:

```sh
cargo build --release --locked --workspace
cargo test --locked --workspace
```

`dist/standalone/` contains the two Rust executables, capability metadata,
checksums and protocol attribution/license files. Previous mixed-profile
binaries are moved to `reference-artifacts/` when the build helper is first run;
they are excluded from the standalone distribution.

Run the frozen-view service with an explicit imported file:

```sh
./dist/standalone/quai-rpc --snapshot fixtures/native/scenario-a.json
```

This view is local and fixed. `/health` reports `standalone-rust`, zero Go runtime
dependency, and `chain_verified: false`. Unsupported state/simulation requests
return an availability error. Gateway/follower/native-probe flags are rejected
by the shipping binary. The product does not silently fall back to Go.

For pinned SDK acceptance, install the locked test dependencies and opt into the
client test; Node/npm are test tools rather than runtime dependencies:

```sh
npm ci --prefix sdk --ignore-scripts --no-audit --no-fund
python3 scripts/verify_rust.py --sdk
```

The expected SDK values are previously recorded direct-Go outputs. No Go server
is started by these standalone tests.

## Native protocol foundation

The pure Rust primitive crate checks bounded work-header commitments and
Blake3 targets, and handles Quai/Qi/ETX transaction Protobuf, routing IDs,
signing messages, ECDSA sender recovery, Qi MuSig2/BIP340 signature mathematics
and typed-RLP encoding. It also supplies all raw schemas and bounded complete
work-object/body/receipt/trie commitments. `quai-execution` implements intrinsic
gas, Qi input checks, in-memory ETX queue roots and bounded isolated VM frames; `quai-network` implements
application framing/correlation/topics. Bounded native settlement, auxiliary proofs and private transport now exist.
Full execution domains, verified hierarchy, durable canonical state and sync
remain open.
The [native VM results](NATIVE_EVM_RESULTS.md) cover account/storage roots,
135 opcode values, gas/refunds/logs and rollback. These are library helpers;
the frozen RPC service does not expose simulation or validated state.
See [the protocol notice](crates/quai-primitives/NOTICE.md) and [scope](PLAN.md).

[Historical native foundation results](NATIVE_FOUNDATION_RESULTS.md) record
6,979 protocol comparisons, 7,490 execution-state/helper comparisons and 34 Rust
tests. [Earlier profiling results](NATIVE_ROUND_RESULTS.md) preserve the
previous allocation/timing run; see the performance ledger for later measurements.
[Earlier HTTP measurements](STANDALONE_SPEED.md) remain historical runs.

Build the Rust-only development profiler and compare preserved binaries:

    cargo build --release --locked -p quai-rpc --example profile-rpc
    python3 scripts/profile_rpc.py --baseline /path/to/baseline/profile-rpc --candidate /path/to/candidate/profile-rpc

Every response is validated. This measures the handler without HTTP or
external client work. Allocation sampling runs separately and measures gross
requested bytes, not live memory. The example is excluded from distribution.

The [matched transaction signature benchmark](TRANSACTION_PIPELINE_RESULTS.md)
compares native Rust with pinned Go using identical frozen inputs and worker
counts. It measures signature pipelines, with no state/database validation.

## Isolated reference lab

The Go-backed prototypes remain development tools behind the explicit
`quai-rpc/reference-gateway` Cargo feature. Build them into a separate source
copy; this does not overwrite the standalone binaries:

```sh
python3 scripts/build_reference.py --go-quai ../go-quai
cd reference-build
python3 scripts/verify_chain.py
python3 scripts/verify_next.py
```

Regenerating protocol vectors is also an explicit oracle task, never a build
step: `python3 scripts/gen_protocol_vectors.py --go-quai ../go-quai`.

[Earlier milestone results](MILESTONE_RESULTS.md), [sidecar measurements](CANONICAL_READS.md)
and [the historical plan](docs/HYBRID_PLAN.md) document the prior architecture.
They are not standalone-client execution benchmarks.

Offline development helpers are explicit and excluded from the shipping bundle:

    python3 scripts/verify_foundation.py
    python3 scripts/verify_foundation.py --interop --go-quai ../go-quai

The second command starts only IPv4-loopback application-wire peers. It does
not connect to a public network or perform libp2p negotiation. Schema/vector
regeneration is also explicit: `scripts/gen_schema.py` and
`scripts/gen_foundation_vectors.py`, each with `--go-quai ../go-quai --check`
or `--write`. Default Cargo builds invoke neither Go nor protoc.

## Native consensus components

`quai-consensus` supplies native ProgPoW/KAWPOW light verification, BTC/BCH
SHA-256d and Litecoin scrypt donor hashing, ordinary difficulty adjustment and
bounded PoEM rule projections. It binds supported non-auxiliary headers to
actual proofs and body commitments. Its full-block admission API returns
unsupported: complete header/uncle/history policy, hierarchy fork choice and economic
state replay remain open. Signed auxiliary bindings now have a qualified
bounded component path.
[Consensus evidence](NATIVE_CONSENSUS_RESULTS.md) records the exact scope.

## Signed auxiliary proof components

Auxiliary-aware library codecs now canonicalize supported donor headers,
AuxPow and signed templates. The consensus crate verifies the pinned 2-of-3
MuSig2 quorum, coinbase/Quai seal/time/Merkle bindings and the Litecoin/Dogecoin
commitment, then recomputes supported KAWPOW block proofs. Quorum keys are
trusted chain configuration; incoming proofs never choose them. The frozen
RPC service does not gain validated chain authority from this library.
[Auxiliary results](NATIVE_AUXILIARY_RESULTS.md) define scope and gaps.

The candidate-history library supplies bounded candidate zone ancestry, workshare controllers and native inclusion checks. See [history results](NATIVE_HISTORY_RESULTS.md). Imported history remains unverified and cannot publish canonical state.

Bounded zone header policy and conversion/base-fee arithmetic now have separate native and controlled-parent evidence: [header results](NATIVE_HEADER_RESULTS.md). Full economic replay and canonical consensus remain open.

Bounded prime/region header policy, efficiency/threshold control, miner-difficulty EMA and ETX eligibility now have separate source and native-proof evidence: [hierarchy results](NATIVE_HIERARCHY_RESULTS.md).


Candidate append preparation (2026-11-10) now computes native order/rank/routing,
checks the local parent manifest or prime interlinks, and prepares PCRC and
manifest/interlink updates without changing supplied history or indexes.
[Placement results](NATIVE_PLACEMENT_RESULTS.md) separate isolated Go policy
projections from native proof/header/body cases. The full admission API remains
unavailable; expansion/genesis qualification, economic replay, competing branches
and durable canonical publication are still required.


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

The experimental genesis-only private execution mode and bounded forward acquisition are documented in [NATIVE_GENESIS_SYNC_RESULTS.md](NATIVE_GENESIS_SYNC_RESULTS.md). Its 8192-block pre-transaction qualification and separate datadir contract do not establish activated/full public consensus or change the frozen RPC boundary.
