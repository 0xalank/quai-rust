# Local proof reuse during native synchronization

The native receiver reuses successful full header proofs from its own acquisition verifier during canonical admission. The opaque in-memory memo cannot be decoded from peers and cannot authorize a body, state transition, receipt or canonical publication. It binds exact normalized header bytes and immutable trusted quorum keys. Failed proofs and weaker inclusion work never become strong proof hints. Native placement, body, history, execution, commitments and fork choice still run.

Acquisition retains its epoch cache across forward windows. Both verifiers share the same immutable locally generated cache allocation; no peer-supplied cache bytes are accepted. Tests check pointer identity, foreign-quorum rejection, changed-header misses, weak/strong separation and entry/byte limits. A valid-work block with an invalid body still leaves only its six valid predecessors durable, then resumes after correct data arrives.

On the fixed 16-block first canonical branch, native admission has **24 full-header proof-cache misses without the handoff and 8 with it**, reusing 16 exact proofs. Both modes produce identical canonical heads and state roots. This is a **66.7% reduction in admission-stage proof-cache misses**, not an overall speedup. No new runtime timing, tracked allocation, heap or RSS gain is claimed.

Fresh native replay verifies **8192 genesis-linked blocks / 12800 context views**, with **122930 fresh-and-known-history checks / 9 writer reopens**. The checker is rebuilt and replayed for this source; no prior executable equivalence is used. Separate Rust processes again import and synchronize all 8192 blocks over enrolled loopback Noise/Yamux. The default v1 profile and experimental v2 contract remain unchanged.

A separate 96-block pilot executes every source block in the Rust checker, reopens that native canonical datadir in a serving node, and synchronizes a second Rust process across a middle-boundary restart. Its **52 assertions** qualify the checker/node datadir bridge, exact receiver roots, native receipt ancestry, two clean receiver reopens and known-head zero-I/O behavior. No supplied state or checkpoint is imported. The generalized driver can consume the completed activation corpus later; its pilot contains zero signed transactions and grants no activation coverage.

The current source-bound release and Go-denied builds pass **281 tests / 228 Rust sources / 24 assets**. The native-network feature passes **13 tests**. Eight fresh optimized MPT stress processes and the explicit inherited-writer-descriptor regression also pass. Existing component, conditional activation, coincident hierarchy, persistence, canonical, process and frozen RPC/SDK checks pass at their recorded scopes. Full global consensus and public sync remain unfinished. The completed 259216-block receiver belongs to an older source build and is documented separately; it is not promoted to current-source coverage. The original full goal remains active.

Reproduce against the completed frozen corpora:

```sh
CARGO_NET_OFFLINE=true python3 scripts/qualify_proof_handoff.py \
  --small-oracle results/genesis-stream-pilot-96 \
  --large-oracle results/genesis-stream-8192 \
  --compatibility-baseline /private/tmp/quai-rust-coincident-20261113
python3 scripts/verify_genesis_processes.py --oracle results/genesis-stream-pilot-96
```

The compatibility argument names the preserved previous qualified source, not a public node. These commands execute Rust validation and controlled localhost peers; Go is used only to generate the explicit offline reference corpora in a separate workflow.

Source-bound evidence: `results/proof-handoff-work-20261116T035653866226Z/summary.json`.
