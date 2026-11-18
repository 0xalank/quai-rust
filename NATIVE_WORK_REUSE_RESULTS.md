# Reusing native full proofs in inclusion and rewards

A successful native block proof now supplies the same exact header's weaker inclusion/reward work. Its full proof already checked mix, target and any mandatory auxiliary quorum binding. Inclusion permits a weaker target and a narrowly defined foreign-donor signature exception, so the full proof satisfies its proof requirements. The reverse direction remains forbidden: inclusion-only work and failures never populate the strong-proof memo.

The normalized header bytes must match. Inclusion memo entries still bind node scope, and native body, ancestry, state, commitments and fork choice still run. Tests remove the epoch cache after full verification and prove that inclusion succeeds without generating another cache; changed headers miss, different scopes remain separate, and weak results cannot become block proofs. The existing cross-quorum handoff guards and valid-work/invalid-body rejection remain intact.

Actual source and receiver processes each record **8189 full-proof-to-inclusion reuses while importing 8192 native blocks**. Each counter increments where another work computation would otherwise run. This is an operation count, not an end-to-end speedup, heap or RSS result. The [prior acquisition/admission reuse](NATIVE_PROOF_REUSE_RESULTS.md) remains included.

The default and reduced Go-denied builds pass **261 release tests / 203 Rust sources / 23 assets**; the native-network feature passes **13 tests**. Fresh 8192-block replay has **122930 fresh-and-known-history checks / 9 writer reopens**. Separate-process sync covers all 8192 blocks in 64 bounded windows, while component, conditional execution, persistence, fork/recovery and frozen RPC/SDK checks retain their exact tested scopes. Disk codecs, profiles, lockfile and official v0.57.0 reference are unchanged.

The incremental verifier also passes an actual **1024-block live Go/Rust pilot**, including **54 temporary EOF waits**, gzip CRC, compressed/raw/source hashes and native roots. Decompressed writes are bounded to **1 MiB**; this is payload accounting, not heap/RSS. An incomplete file grants no acceptance. The original producer is never restarted or altered by the verifier.

A second Rust process then synchronizes from that already-executed native datadir across a boundary restart. Its **62 assertions** cover exact heads/roots, native receipts, recovery and known-head zero-I/O behavior. The source is reopened directly; no state image/checkpoint is imported and its execution is not repeated. Three incomplete/wrong-build/wrong-head source-proof cases fail before creating a datadir or process and preserve every source database file.

The full 259216-block producer and native follower remain separate active experiments. Their mutable output is excluded from installation and grants no completed activation coverage. The private RPC boundary stays frozen/unverified; full global consensus and public canonical sync remain unfinished. The tracked full goal is active.

Reproduce the qualified slice from its preserved source and completed corpora:

```sh
CARGO_NET_OFFLINE=true python3 scripts/qualify_strong_work.py \
  --small-oracle results/genesis-stream-pilot-96 \
  --large-oracle results/genesis-stream-8192 \
  --compatibility-baseline /private/tmp/quai-rust-coincident-20261113
python3 scripts/verify_followed_genesis_processes.py \
  --source-proof results/follow-genesis-20261116T040155111026Z/summary.json \
  --scratch /private/tmp/quai-followed-receiver-new
```

The second command requires the retained locally executed source datadir named by its proof; it never substitutes supplied state. Earlier commands and measurements belong to their archived source versions. These workloads are private and pre-transaction; all ETX kinds, later forks, expansion, full coordinator/EVM/resources, public following and validated live RPC remain open.

Source-bound evidence: `results/work-reuse-20261116T044232200275Z/summary.json`.
