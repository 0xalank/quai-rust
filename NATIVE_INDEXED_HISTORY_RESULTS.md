# Branch-bound ancestry and native hierarchy recovery

The combined catalog integration and current private directory format are documented in [indexed catalog results](NATIVE_INDEXED_CATALOG_RESULTS.md). This report preserves the ancestry increment and its earlier catalog boundary.

The native private canonical path now loads ancestors, placement snapshots and hierarchy confirmation output from a pinned database version on demand. Rebuilding confirmation components no longer replays every ancestor from genesis. Global full consensus and public canonical synchronization remain unfinished; the tracked goal stays active.

Each successful native hierarchy preparation persists its resulting context heads, manifests, interlinks, termini, retained region rollup and newly confirmed ETXs in the same transaction as that block's execution state. These local outputs remain bound to the version metadata digest, profile and raw work object. They are not accepted from peers. Order-two blocks retain no region rollup, matching the native hierarchy's commit behavior.

A reader captures one immutable database read transaction and anchor. A lookup resolves the requested block to a version, then proves that version is an ancestor of the anchor using persisted binary links. It returns no record for another fork or a future descendant. Missing or corrupt known versions, malformed hierarchy output and mismatched context witnesses fail instead of becoming empty or zero state. Coincident context views must bind to the same block and body/work headers. Old pinned readers preserve their branch across a canonical reorg.

The ancestry API returns shared records rather than deep copies at every walker step. A recent native hierarchy overlay is retained for at most 64 blocks before it rebases to persisted output. If a smaller existing byte/entry bound fills sooner, the same candidate is retried once against a fresh reader of the identical persisted parent; candidate limits and validation remain unchanged. A native 16-block test forces three rebases using a four-block cache window and checks EVM/ETX commitments at every block. Imported candidate overlays remain bounded. A generic history reader caches at most 512 records / 16 MiB of encoded bytes; a placement reader caches at most 256 snapshots / 8 MiB of estimated content; each database-backed source caches at most 32 decoded versions / 16 MiB of metadata payload. Within each immutable database read transaction, at most 128 verified version headers are shared across binary ancestry queries; a fresh transaction rechecks storage. The reviewed profile/genesis is decoded once and shared immutably. A dedicated test checks cache bounds, pinned consistency and corruption rejection by a fresh reader. Cache limits bound retained entries and charged content, not exact heap or RSS. Oversize entries bypass the cache. A cache miss may perform database reads and decoding; execution maps and a configured database cache are additional costs.

The private directory format changes to canonical version 3, keyed branch marker 2 and branch metadata 4, with hierarchy delta codec 1. Execution state codec 1 remains unchanged. Earlier private directories are preserved and refused. Use a fresh private datadir; no automatic migration is provided.

## Evidence and limits

New tests cover exact recovery of native hierarchy output at all three orders, malformed/truncated/trailing/wrong-ID/parent data, branch exclusion after a reorg and immutable pinned reads. In the actual native 16-block branch, initial recovery decodes one tip metadata record; constructing the hierarchy provider performs no further ancestry decodes. This is a record-loading count, not a complete startup CPU, disk or RSS measurement. State restoration still materializes bounded maps and the canonical catalog still reconstructs its receipts.

A separate synthetic candidate-reader test performs 6,000 distinct record lookups while retaining at most 512 cached records. Those synthetic IDs and heights are not admitted native blocks or evidence of a 6,000-block network history. The current canonical admission catalog and early profile still have their 4,096-record and aggregate byte bounds. Removing those requires indexed canonical receipts/frontiers and broader profile qualification, not increasing a cache constant.

The unchanged frozen Go corpora remain the authority for consensus outputs. The canonical checker still compares native head, fork choice, state roots, receipts and recovery with the same 24-block corpus. Separate node-process qualification checks the indexed mode alongside durable selected versions, reorg/restart, SIGKILL and cancellation behavior. The complete default and reduced Go-denied builds each pass **219 release tests over 183 Rust sources / 22 source assets**, with zero forbidden-tool attempts. The canonical checker retains **488 Go comparisons / 24 blocks / 25 events** and separate Rust processes pass **149 assertions**. Persisted signed Quai/Qi/incoming components retain **18,312 Go comparisons across 1,371 cases**, with **2,706 state roundtrips**. These overlap the original component checks and are not a completion percentage. Frozen RPC/SDK comparisons remain **70/16**. Exact source-bound reports are recorded in `results/latest-indexed-history-work.json`.

The before/after benchmark uses two separately compiled Rust builds, independent Cargo targets, the same unchanged harness and 16-block hierarchy/coinbase input, and the same keyed database configuration. Seven alternating sequential timing pairs and three separate allocation pairs measure warm native execution, fresh parent restoration, staging and durable branch-head selection. Initial database/cache allocation, parsing, genesis setup, proof warming and final validation are excluded. This does not measure canonical catalog startup, long-history scaling, public sync, Go performance, retained memory or RSS. Raw values, regressions and source/binary hashes are retained in `results/latest-indexed-history-performance.json` and the performance ledger.

Lazy account/storage/code, Qi and ETX state, indexed canonical admission and frontier recovery, receipts/log indexes, pruning and production storage faults remain open. Broader mixed-transaction/fork/nonlocal/expansion consensus, complete coordinator workers, automatic public sync and live verified RPC also remain open. `Verifier.validate_block` continues to return Unsupported.

## Reproduction

    python3 scripts/build.py
    python3 scripts/verify_rust_build.py
    python3 scripts/verify_persistent_execution.py
    python3 scripts/verify_canonical.py --peer
    python3 scripts/verify_private_processes.py
    python3 scripts/build_private_node.py
    python3 scripts/benchmark_indexed_history.py --baseline /absolute/path/to/frozen/previous/source

Keep the previous qualified source and all benchmark inputs frozen throughout the comparison. Normal build, distribution and runtime require no Go checkout or toolchain. The optional node runner remains confined to enrolled loopback peers and a fixed private profile; the default RPC remains read-only and frozen.
