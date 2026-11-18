Current work: [local-zone header/execution results](NATIVE_ADMISSION_RESULTS.md). The counts and timings below describe the preceding source increment. Full consensus and canonical sync remain unfinished.

# Durable private execution branches

Full consensus and canonical synchronization remain **unfinished**. Official v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the reference; the local fee overlay stays inactive. RPC continues serving a frozen read-only unverified view.

`quai-consensus::private_branch` now persists complete local state images together with block bytes, typed receipt bytes, created UTXO keys and deleted hashes in the private journal. Recovery rebuilds branch-specific block/redemption/trim history. Native proofs and atomic execution run before staging a candidate. Choosing the private execution head is explicit; it does not run PoEM selection or mint a full admitted-block token. Inbound eligibility and full header/hierarchy policy remain unqualified inputs.

The new actual-Go oracle has two 16-block branches sharing genesis through height eight, then different signed storage writes. There are **24 unique blocks** across 32 rows. Native work hashes, account/ETX/UTXO commitments, account/UTXO counts, typed receipts and restored incoming history match Go. Eighteen normal selected-head reopens, an eight-block undo/forward switch and return, stale selection, malformed candidates and corrupt selected/unselected objects are checked. The checker records **289 comparisons**. Process-exit tests separately recover the full last-selected state/receipts/history with a staged versus selected candidate; existing journal tests cover staged/published and torn-file boundaries.

The full default suite passes **179 tests**. Reduced Go-denied verification covers **145 Rust sources and 20 source assets**. Existing component comparisons remain 40,511; with the new durable checks the total is **40,800 component comparisons**, not a functional completion percentage. RPC/SDK compatibility remains **70/16** checks. [Combined source-bound verification](results/durable-work-20261111T213815711963Z/summary.json), [branch differentials](results/private-branches-20261111T213310312884Z/summary.json) and [Go-denied build](results/durable-work-20261111T213815711963Z/rust-no-go-build.json) preserve their scopes.

## Journal optimization

The previous journal repeatedly read/checksummed the same ancestry and cloned payloads while only examining parent/height metadata. The new implementation checks each unique object once per switch operation and borrows its metadata. The traversal memo is discarded after the operation; later calls reread/checksum snapshots. Tests corrupt an already checked ancestor and require the next switch to fail without changing HEAD. Both branch lineages retain height/genesis/depth checks, writer exclusion, stale-head compare-and-swap, content addressing and fsync/rename publication.

Five alternating sequential timing pairs used two height-63 tips sharing 48 checkpoints, with fixed 256 KiB opaque snapshots and eight head switches per process. Filesystem cache is warm. Timings include checksum scans/traversal and head fsync/rename. Three separate allocation pairs measure gross Rust allocator requests; reallocations count the full requested new size.

| Fixed journal workload | Previous Rust median | Current Rust median | Improvement |
| --- | ---: | ---: | ---: |
| Head-switch latency | 184.918531 ms | 79.460948 ms | 2.33x |
| Gross allocated bytes per switch | 272,838,480 | 83,989,872 | 69.2% fewer |
| Allocation requests per switch | 11,482 | 4,374 | 61.9% fewer |

[Raw samples, contracts, source/binary hashes](results/journal-performance-20261111T213425497812Z/summary.json) and [complete baseline source](results/journal-performance-20261111T213425497812Z/baseline-source.tar.gz) are retained. These measure the private journal on a shared host, not the full branch wrapper, cold-device I/O, peak/retained RAM, block TPS, RPC or end-to-end sync. The [performance ledger](PERFORMANCE_LEDGER.md) keeps these separate from earlier state and cache-hit results.

## Remaining scope

Only the reviewed empty-allocation private chain profile is accepted. Commits are limited to 63 MiB and recovery to 4,096 branch records. Legacy early-checkpoint payloads are rejected rather than migrated silently. Restores trust the local private content-addressed head; there is no peer snapshot import. Complete images are snapshots rather than a production incremental/pruned state database. Recovery still reads ancestor payloads to rebuild history; a separate durable metadata/history index remains work.

Next gates are broader mixed Quai/Qi/ETX and auxiliary-workshare/fork histories; verified hierarchy/inbound eligibility; sealed full block admission; production branch/index transactions and crash qualification; and PoEM-driven canonical publication/reorg/sync. The private transport's earlier three-block replay is separate evidence. No public peer, mining or wallet operation was started.

Reproduce native checks after building:

    python3 scripts/verify_private_branches.py
    cargo test --locked --offline -p quai-consensus --test private_branch
    python3 scripts/verify_rust_build.py

Explicit offline oracle regeneration is `scripts/gen_private_branch_vectors.py --go-quai ../go-quai --check`. Ordinary Rust builds/runtime never invoke Go. Installation, rollback and bounded readable content/metadata inspection are recorded in `results/durable-installation.json`.
