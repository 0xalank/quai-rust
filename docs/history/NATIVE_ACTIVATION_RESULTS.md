# Sealed local admission at transaction activation

Full consensus and public canonical sync remain unfinished. Official go-quai v0.57.0 remains pinned. Go is an explicit offline oracle and stays absent from the default Rust build, distribution and runtime. Global `Verifier.validate_block` remains Unsupported and public RPC remains frozen, unverified and read-only.

## Composed checks and handoff

Local append checks can consume an explicit parent branch index, then check native proof/order, zone header/body policy, manifest, share entropy and PCRC. A separate activated local hierarchy entry checks the accumulated cross-prime ETX rollup and derives prior confirmed inbounds. It is restricted to activated, prime-zero, local-order components; other external transaction kinds and dominant/later-fork cases remain outside that entry.

Execution can consume a prepared hierarchy result bound to the exact candidate bytes and the same reviewed chain/location/genesis/allocation policy. Private execution selects the empty-allocation policy at compile time; official execution retains its allocation-table specialization. Callers cannot replace its inbound list. The existing private canonical execution path uses this shared handoff and commits the graph prefix only after durable execution staging succeeds. Preparation and staging still grant no independent publication authority; the existing canonical receipt/publication protocol remains responsible for selected generations.

## New frozen Go corpus

The offline oracle constructs an explicit synthetic checkpoint at zone height 259,200, four preceding checkpoint records, seeded account/UTXO/ETX state and explicit empty historical redemption/creation slices. These are supplied test inputs, not validated history from genesis. No public peers, real wallets or mining service are used.

Go accepts four newly sealed local blocks at heights 259,201 through 259,204 using actual `AppendHeader`, `ValidateBody`, `StateProcessor.Process`, `ValidateState`, PCRC, committed rollup validation and local confirmed-ETX derivation. The first block contains a signed Qi spend, signed Quai CREATE and a standard queued ETX. Subsequent blocks execute signed SSTORE calls. Receipts, account/ETX/UTXO roots, fees, gas/state counters, reward-depth payouts, manifests and termini use the pinned upstream rules.

Thirty-three freshly sealed mutations pass native header/body checks and fail the applicable execution, state-commitment or rollup check. Cases include a Quai nonce failure after a Qi spend, an invalid Qi signature, a double spend, fee ordering, the wrong queued ETX, gas/state counters, account/queue/UTXO/receipt roots and rollup commitments. The rollup mutations demonstrate why header and execution checks alone cannot establish complete admission.

Rust replays the corpus in memory and from lazy persisted state, rejects all mutations atomically and preserves the selected execution version on failure. Additional tests bind prepared results to the exact block and chain, reject missing historical slices as unavailable, and confirm the synthetic checkpoint cannot enter the canonical node as an admitted parent. The private runtime's early profile is unchanged; the four conditional blocks must not be added to its 24-block-from-genesis count.

## Qualification and performance

The final default and reduced Go-denied builds pass **252 release tests / 198 Rust sources / 22 assets**. The new conditional suite has **772 checks**, with **66 rejected attempts / 9 state roundtrips / 8 writer reopens** across two backends. Existing qualification retains **49,144 component checks**, **18,312 persisted Go comparisons**, **488 canonical checks / 24 genesis-linked private blocks**, **165 process assertions**, **70 RPC / 16 SDK comparisons**, **11 older-format refusal assertions** and **9 unchanged-format compatibility assertions**. Source-bound evidence is in `results/latest-activation-work.json`. Disk formats and dependency lock are unchanged.

This increment qualifies consensus composition and removes the handoff's inbound-list clone. No new timing or allocation improvement is claimed. Earlier measurements in the [performance ledger](../../PERFORMANCE_LEDGER.md) belong to their recorded prior source hashes and workloads.

## Reproduce the conditional slice

From the project root, rebuild and run the frozen activation corpus without Go:

```sh
CARGO_NET_OFFLINE=true python3 scripts/build.py
python3 scripts/verify_activation.py
```

The explicit offline oracle check is `python3 scripts/gen_activation_vectors.py --go-quai ../go-quai --check`. It requires the pinned Go checkout/toolchain and is excluded from the default build/runtime. The source archive, build manifests, immutable checker binary and raw logs are retained with the qualified work report. Reproducing this slice alone does not qualify the rest of the client or promote a network gate.

## Remaining work

Authenticate complete prior history and broaden the composed path across dominant/sibling histories, every incoming ETX kind, auxiliary/later economic forks and expansion. Integrate a generally configured canonical admission/coordinator path and automatic sync; finish full EVM/resource, sustained scale, pruning/index, Linux/fault, public shadow-target and live state RPC gates. The new checkpoint corpus is conditional component evidence, not a completed full-consensus or network gate.

Qualified private-node file sizes: 11284720 bytes in the initial handoff draft, 7916240 bytes after allocation-policy specialization, versus 7915920 bytes in the preceding installed source. This is an on-disk artifact comparison; it does not measure runtime latency, allocations or RSS. See `results/activation-layout-20261113T151719369084Z/summary.json`. The official allocation asset is preserved for official execution and source qualification.
