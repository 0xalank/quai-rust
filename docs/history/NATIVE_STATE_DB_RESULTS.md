# Incremental trie roots and durable state versions

The native Rust client now updates account-state commitments by copying changed secure-trie paths. A separate transactional state database persists immutable trie nodes, roots, version headers, metadata and block indexes. **This report records the earlier standalone backend qualification. The [subsequent canonical integration](NATIVE_CANONICAL_DB_RESULTS.md) now uses it in native private execution; the full-consensus/canonical-sync goal remains active.** Official go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the reference, with the preserved local fee overlay inactive.

## Implementation and storage contract

`quai-primitives::mpt` implements fixed hashed-key secure MPT lookup, insertion, deletion, path splitting/collapse and canonical RLP encoding. Initial snapshots build once in sorted hashed-key order; subsequent mutations copy paths. Immutable branches use shared ownership; untouched persisted branches load through a caller-owned hash-addressed reader. Missing, corrupt and noncanonical accessed nodes return errors. Empty values delete keys. Inline-child and long shared-prefix boundaries are tested. Existing ordered/raw/snapshot trie implementations remain independent references.

`World::root` now advances from its last committed in-memory trie and updates only dirty accounts. Derived commitment caches are excluded from state equality and checkpoint images. Unchanged snapshots share the derived root cache; account writes detach it before mutation. Snapshot roots remain stable across divergent writes. Aggregate slot/byte quotas still apply, including after a rejected root and subsequent valid changes. Shared root-cache publication is safe only for identical account snapshots; every account mutation detaches before writing, and tests compare divergent roots with independent full recomputation. The account map still copies on first shared write and each changed account still recomputes its storage root; this increment does not provide lazy account/storage execution. Shared trie nodes and cached encodings retain additional data; no allocation or RSS reduction is inferred from the root algorithm.

`quai-store::state_db` uses the pinned Rust redb 4.5.0 backend. One immediate-durability transaction commits new content-addressed trie nodes, a small immutable version header, content-addressed metadata, the block-to-version index and an optional selected-head compare-and-swap. Callback errors and stale-head expectations roll back the whole transaction. Fork versions and read transactions retain independent roots; selection cannot rewrite their history. Version headers separate large metadata and include binary ancestor links, so ancestor traversal does not deserialize complete state images or scan every height. Trees can share persisted child storage roots in the same transaction.

Node counts and encoded bytes below describe newly inserted unique logical records. They are not physical disk writes, write amplification, peak memory or RSS. Nodes and versions are retained; garbage collection/pruning is not implemented. Existing database schema/profile mismatches, unavailable references and conflicting immutable records fail closed; unknown databases are not silently migrated. Integrity checks cover version/metadata digests and nodes as accessed, not an exhaustive scrub of every retained descendant at each open.

## Actual-Go qualification

The explicit offline oracle invokes pinned Go `trie.TryUpdate`, `TryDelete`, `TryGet`, `Hash`, commit and root reopen. The normal Rust build consumes frozen outputs and never needs Go. The differential verifier covers **6,290 comparisons**:

- 1,512 mutation steps with per-step roots/lookups, deletion, retained roots, hash-linked reloads, deep prefixes and value-encoding boundaries.
- 70,000 secure keys with intermediate Go roots, then 100 changes/deletions checked against Go while the old root remains readable.
- 5,000 additional durable state-version commits, retained historical reads, selected metadata and binary ancestor lookups after reopen; 5,063 total versions.

The 70,000-key import inserts 95,792 unique trie nodes / 6,998,875 encoded node bytes. Its 100-key change inserts 341 unique nodes / 100,141 encoded bytes. The workload contains 70,000 existing keys in both cases. Those figures illustrate the recorded incremental change footprint, not a timing or whole-client disk reduction. The final database file in this fixed run is 40,374,272 bytes; redb page/allocation overhead and retained history are included.

Unit tests separately cover independent forks and pinned reads, nested storage/account roots, stale-head and callback rollback, missing foreign node references, later node corruption, metadata corruption, profile mismatch, and separate processes exiting immediately before/after durable commit. Recovery yields the complete old or new selected version and matching root/block index. These controlled process exits do not qualify power-loss behavior on every filesystem/platform.

The current default and reduced Go-denied builds each pass **208 release tests**, with **176 Rust sources / 22 source assets**. The database comparisons are additional storage evidence, separate from the existing 49,144 overlapping execution/consensus component comparisons. Neither count is a protocol completion percentage. Private canonical and separate-process sync checks rerun against the same new execution-root source; their fixed early private profile remains unchanged.

## Reproduction

    python3 scripts/build.py
    python3 scripts/verify_state_db.py
    python3 scripts/verify_rust_build.py
    python3 scripts/verify_canonical.py --peer
    python3 scripts/verify_private_processes.py

Explicit oracle regeneration, outside the shipping build:

    GOCACHE=/private/tmp/quai-benchmark-go-cache GOPROXY=off GOTOOLCHAIN=local python3 scripts/gen_state_db_vectors.py --go-quai ../go-quai --check

See `results/latest-state-db.json` for source/corpus/binary-bound results. The [paired performance experiment](../../results/incremental-state-performance-20261113T071555851404Z/summary.json) measures a 10.82x changed-account/root speedup and 0.97x first-ever-root ratio. It has its own frozen harness, exact baseline source, raw samples and source/binary hashes in `results/latest-incremental-state-performance.json`; the performance ledger records improvements, regressions and rejected runs. The first guard clones a parent and can reuse shared derived caches; the first-ever-root guard constructs fresh independent state before each sample. Neither includes setup in its timer.

## Remaining integration at this checkpoint

At this earlier checkpoint, private canonical admission still persisted complete local images. The subsequent [canonical integration](NATIVE_CANONICAL_DB_RESULTS.md) replaces those images with keyed database state; the 4,096-record and aggregate-history limits remain. The new 5,000-version storage test does not remove those client limits and does not represent 5,000 admitted blocks. Persistent lazy account/storage/code, Qi/UTXO/ETX state, validated block/receipt/log indexes and transactional canonical publication must be integrated with this backend, then qualified together over sustained forks, restart and faults. Production pruning, live state RPC, automatic head following, broad mixed/fork/hierarchy admission and a reviewed public shadow target remain open. Global `Verifier.validate_block` stays Unsupported, and readiness gates remain partial except the Rust-only build boundary.

No public Quai peer, production mining or wallet operation is part of this qualification. Go is used only by the explicit offline oracle. The default RPC remains a frozen read-only unverified view. Readable content/metadata inspection and reversible installation evidence are recorded with the installed result.
