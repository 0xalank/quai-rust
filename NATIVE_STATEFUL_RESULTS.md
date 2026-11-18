Current work: [consensus/execution results](NATIVE_CONSENSUS_RESULTS.md). The counts below describe the preceding source increment. Full consensus and canonical synchronization remain unfinished.

# Stateful Rust client build-out

This increment follows the five-step private-client plan against official
v0.57.0 source `33db82f2891b267ba898ca38bd2dd8bf75cbb072`. The archived local fee
extension stays inactive. Go is used only by explicitly invoked offline oracles.
The default build, distribution and runtime remain native Rust.

The RPC executable still serves a frozen imported read-only view. The new code
is a bounded execution, storage and private transport library increment. Full
consensus, general historical block replay and canonical network sync remain open.
No performance benchmark was run for this increment.

| Plan step | Implemented and checked | Remaining acceptance |
| --- | --- | --- |
| 1. Freeze private configuration | Chain 1337, prime/region/zone hierarchy, three Go-matched genesis payloads, fork/source hashes, engine choices, empty allocations and loopback-only transport in [private-native-v1.json](configs/private-native-v1.json). Exact reviewed bytes are fingerprinted. | Full allocation/economic and historical network qualification. This private contract is separate from the unresolved production target. |
| 2. Journal and recursive execution | Granular nested revisions, balances/nonces/code/storage/transient state, original storage, logs/refunds/ETX rollback, CALL families, CREATE/CREATE2, nine precompiles and touched-empty-account finalization. | Complete valid historical domains, resource qualification and the lockup precompile lifecycle. |
| 3. Transaction and Qi/ETX settlement | Authenticated signed Quai transactions, gas/nonce/fee/refund accounting, receipts, creation failure behavior, EOA selfdestruct forks, atomic Qi spends/outputs/fees/signatures, conversion/wrapping policy, outbound ETX/CONVERT and exact UTXOHash/MuHash3072 commitments. | Incoming ETXs, coinbase/lockup side effects, KQuai authority state, genesis allocations, reward-depth payouts and historical trimming. |
| 4. Whole-block differential replay | Three linked early private CREATE/SSTORE blocks through actual Go StateProcessor.Process, matched account/ETX/UTXO commitments, receipt bytes, gas/state counters and state size. A rebound header with a forged gas counter is rejected without state mutation. | General block replay across forks, incoming ETXs/rewards and composition with native proof/hierarchy admission. |
| 5. Durability and private transport | Immutable private execution checkpoints, writer lock, atomic fsync/head compare-and-swap, ancestry/reorg planning, corruption rejection and process-exit recovery. Actual enrolled loopback libp2p Noise/Yamux persistent streams, identify/Kademlia and manual gossip validation. Early blocks delivered, checked and recovered after three store restarts. | Persistent peer identities/peer policy, full fork choice and sealed block admission, validated canonical commits and full private/public sync. |

The source-bound component report is [latest-stateful.json](results/latest-stateful.json).
The full default suite passed 161 tests. Frozen source differential checks cover:

| Workload | Cases | Comparisons or checks |
| --- | ---: | ---: |
| Nested VM, CREATE, precompiles, outbound ETX/CONVERT | 1,233 | 14,796 |
| Signed Quai settlement and actual transaction finalization | 192 | 2,493 |
| Actual ProcessQiTx, UTXO/MuHash, fees and outbound ETXs | 756 | 9,483 |
| Three linked early blocks and local snapshot/rejection controls | 3 | 40 |

Go formats a pointer instead of its limit value in the two Qi ETX-budget
error messages. The oracle retains the raw diagnostic in stderr and replaces
only that process-local pointer value in frozen diagnostic text; actual limits,
accept/reject, state and execution fields are retained.

The last row includes negative and checkpoint controls; it is not forty independent
blocks or forty proof/hierarchy checks. The block oracle uses the actual state
processor while supplying execution ancestry/order context; it does not establish
valid proof-of-work or canonical hierarchy admission.

The [Go/Rust transport report](results/native-p2p/summary.json) records eight
actual `/quai/1.0.0` protobuf request/response pairs over Noise/Yamux. The
[private replay report](results/private-replay.json) records three valid early
block deliveries, one invalid execution commitment rejected and three durable
store restarts. The transport tests additionally verify frame limits, stream
poisoning after failure, enrollment and three-peer gossip withholding until
application acceptance. Rejected gossip is not forwarded. Public addresses are
blocked before TCP dialing. These are private development tests, not full sync.

The scratch store is separate from frozen RPC storage. It verifies local snapshot
integrity and lineage; it never certifies network consensus. Recovery tests cover
process exit at staged/published boundaries and incomplete temporary files.
Filesystem/device power-loss guarantees beyond the host fsync/rename semantics
have not been established.

From the project directory, reproduce the Rust checks with:

```sh
python3 scripts/build.py
python3 scripts/verify_stateful.py
python3 scripts/verify_rust_build.py
cargo test --locked --offline -p quai-network --features native-transport
cargo run --locked --offline -p quai-network --features native-transport \
  --example private-replay -- fixtures/protocol/early-blocks.json /tmp/quai-private-new-store
```

The last command needs a fresh dedicated store directory and starts enrolled
loopback peers. Explicit Go corpus regeneration/checking and Go/Rust interop use
`scripts/gen_stateful_vectors.py`, `gen_settlement_vectors.py`,
`gen_qi_settlement_vectors.py`, `gen_early_block_vectors.py` and
`verify_native_p2p.py`, each with an explicit `--go-quai` source path. Those
reference tools are excluded from the default build and distribution.

The next integration work is incoming ETX/lockup execution and complete economic
state, then general fork-boundary block replay and sealed admission combining
proof, hierarchy, fork choice and every execution commitment. Canonical storage,
full private sync and live-state RPC depend on that admission result.

[PORTING_MAP.md](PORTING_MAP.md) and [network gates](compatibility/network-gates.json)
track these remaining requirements without a completion percentage.
[Known stateful qualification work](compatibility/stateful-limitations.json)
records specific missing historical and nested-execution cases. Existing
performance reports keep their original revision/workload scope. Dependency
attribution is retained in [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt);
[license provenance](licenses/upstream/sources.json) distinguishes fetched pinned
upstream notices from packaged MIT declarations where no standalone file existed.
