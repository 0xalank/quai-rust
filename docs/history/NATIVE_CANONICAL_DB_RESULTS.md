# Canonical execution on the native state database

This report records the earlier storage-integration baseline. Current branch-bound ancestry recovery, counts and the next private storage format are documented in [indexed history results](NATIVE_INDEXED_HISTORY_RESULTS.md). Measurements below retain their original source and workload.

The optional native private node now stages and restores execution state through the transactional trie database. Its canonical path no longer serializes complete execution-state images. Native proof/header/body/hierarchy checks, derived incoming transactions and native frontier selection still authorize publication. **Global full consensus and public canonical sync remain unfinished; the tracked goal remains active.** Official go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the pinned source, with the archived fee overlay inactive.

## State layout and publication

The keyed execution codec stores account consensus values, local account metadata, storage preimages and child tries, content-addressed code, ETX consensus/queue/auxiliary records, lockups, raw UTXOs and independent MuHash/count metadata. Each local preimage is checked against its hashed key. Code hashes, storage roots, complete account/ETX roots and logical counts are checked again on restoration. Missing code or storage preimages fail restoration instead of becoming empty state. Legacy Qi duplicate-overwrite/delete behavior remains explicit: raw membership and the independently committed accumulator/count are preserved separately.

A completed native transition writes changed records and new trie nodes in one immediate-durability database transaction. Its metadata is prepared inside that same transaction and binds the resulting named-root set, profile/anchor, raw work object, receipts, created/deleted historical records and hierarchy witnesses. There is no complete state-image blob in this format. New candidate versions remain unselected until native canonical admission publishes a durable catalog receipt and advances the database's selected-head compare-and-swap. Recovery validates those receipts and selects the highest admitted private frontier; staged versions alone cannot grant authority.

A prepared permit triggers fresh restoration before publication. Replacement of the open database pathname is detected through its Unix file identity. Store-level tests also cover corrupt nodes/metadata and pinned read consistency. Read transactions use the database's MVCC/page cache; these checks are not an exhaustive physical-disk scrub on every operation. Initialization can resume a committed but unselected genesis. Older image directories are preserved and refused by keyed opening. The private canonical format is now version 2, branch metadata version 3 and keyed execution state version 1; there is no automatic migration of earlier datadirs.

The old complete-image branch/codec remains available for its existing Rust qualification and the explicit storage comparison. The native canonical constructor chooses the keyed database.

## Qualification

The full default and reduced Go-denied builds each pass **214 release tests** over **181 Rust sources / 22 source assets**. New tests cover keyed state roundtrips, missing code/storage indices, legacy MuHash ghosts, genesis recovery, directory preservation and bounded trie iteration. Existing private canonical admission, invalid-candidate atomicity, immutable views, reorg and crash tests now execute on the database backend.

The native canonical checker retains **488 actual-Go comparisons across 25 events / 24 distinct blocks**, a higher-entropy shorter-branch reorg, three reopens and eight invalid candidates. Its head and EVM/ETX/UTXO commitments remain unchanged. **133 assertions** across independent Rust node processes qualify peer acquisition and restart/kill/cancellation recovery using the same new storage path; current source-bound evidence records their assertions and backend/durable-head checks.

A new persisted execution checker runs the existing signed Quai, Qi and incoming-ETX corpora from persisted initial state, then restores completed results before the original Go comparisons. **1,371 cases / 18,312 Go comparisons** pass, with **2,706 state roundtrips / 2,707 stored versions** and 36 rejected incoming transitions. These are component state versions, not admitted blocks. Their comparisons overlap the original 49,144 component comparisons and are reported separately. The generic 70,000-key / 5,000-history-commit database qualification still passes its 6,290 comparisons.

Both original and persisted checkers retain the same Go reference, inputs, gas/receipt/output comparisons and rejection expectations. No Go executable is needed by the normal build or runtime. Frozen RPC/SDK checks remain 70/16; RPC continues to serve an unverified read-only snapshot.

## Efficiency and limits

State writes now address changed logical keys; ancestral branch loads read version metadata instead of repeatedly loading embedded complete state images. The fixed storage comparison uses one current Rust binary with explicit `images` and `database` constructor arguments. Both modes run the same sixteen-block hierarchy/coinbase workload and warm proof verifier; initial database/cache allocation and parsing are outside the timer. Timing and gross allocation runs are separate. This is a new configuration comparison, separate from the historical root-kernel experiments. The [paired comparison](../../results/state-storage-performance-20261113T082724557472Z/summary.json) measured **22.415 → 11.543 ms/block (1.94x faster)** and **19.9% fewer gross requested bytes**, with **2.46% more allocation requests**. The performance ledger preserves raw samples, excluded cache/startup costs, tradeoffs and exact scope.

Execution still materializes bounded World/UTXO/ETX maps on restoration, scans ancestry and reconstructs hierarchy state. The database has a configured 64 MiB cache; this is not measured RSS and is additional to execution maps and other allocations. Current World, per-account storage, queue, UTXO and materialization budgets remain. Catalog/history still have 4,096-record and aggregate-byte limits. The 63 MiB per-version metadata limit now covers work/receipt/witness metadata rather than a complete execution image. Old bytecode and immutable versions/nodes are retained; pruning is unfinished. This increment does not claim unbounded lazy execution, full storage/index parity, whole-node TPS or production memory/disk qualification.

Remaining work includes lazy account/storage/code and Qi/ETX providers, database-backed ancestry/catalog/frontier queries, receipt/log indexes, pruning and sustained fault/scale qualification. Full mixed-transaction/fork/nonlocal/expansion admission, complete coordinator workers, automatic public sync and a reviewed public shadow target remain open. `Verifier.validate_block` stays Unsupported until those integrated gates pass.

## Reproduction

    python3 scripts/build.py
    python3 scripts/verify_persistent_execution.py
    python3 scripts/verify_canonical.py --peer
    python3 scripts/verify_private_processes.py
    python3 scripts/verify_rust_build.py
    python3 scripts/benchmark_state_storage.py

Run the benchmark only after functional checks and other compilation/tests stop. Actual peer tests use enrolled loopback sockets. No public Quai peer, production mining, wallet key or funded operation is part of this work. Use a fresh private datadir for the separately packaged node. Source-bound results are pointed to by `results/latest-canonical-db-work.json`; installation/rollback and readable content/metadata inspection are recorded in `results/canonical-db-installation.json`.
