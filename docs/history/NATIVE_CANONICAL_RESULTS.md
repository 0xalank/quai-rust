# Native private canonical admission and sync

This report records the earlier canonical-publication checkpoint. [Current canonical storage results](NATIVE_CANONICAL_DB_RESULTS.md) now qualify keyed transactional execution state and independent-process recovery; global consensus/public sync remain unfinished.
The fixed early private profile now has native admission, deterministic frontier selection, atomic canonical publication, pinned generation reads, crash recovery and raw-block acquisition over actual Noise/Yamux peers. **Full global consensus and public canonical synchronization remain unfinished.** Official v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the offline reference. Go is absent from default build/distribution/runtime. The shipping RPC remains a frozen unverified read-only view.

## Composed native path

`PrivateCanonical` accepts raw zone objects and required prime/region context witnesses. The fixed hierarchy profile checks native work/order, header/body/placement, manifests/interlinks/PCRC, strong/weak pre-fork share entropy, confirmed rollups and derived incoming coinbase execution. A successful staged result creates a store-bound `PrivateAdmittedBlock` permit. No peer can supply a trusted permit, score, state image or checkpoint. Publication rechecks staged immutable state, then binds ancestry, roots and native frontier score in a checksummed immutable receipt.

The underlying journal HEAD is the single selected commit pointer. The admission receipt is written and synced before publishing HEAD. Recovery ignores a staged checkpoint with no receipt and rolls forward a complete admitted receipt whose HEAD update was interrupted. A failed commit poisons the live publisher until reopen. Exclusive writer locking, profile fingerprints, strict catalog encoding and size limits prevent silent migration or ambiguous recovery. Corruption after admission cannot be hidden by a cached permit.

An immutable `CanonicalView` retains its generation through a reorg. Binary ancestor links provide O(log height) canonical height lookup without rewriting the full height map. The implementation supports at most 4,096 admitted records, 128 MiB of catalog records and 32 MiB of union zone history; individual raw objects are limited to 3 MiB. These are private prototype resource limits, not mainnet storage capacity. It still stores complete local state images and scans/rechecks bounded ancestry during recovery.

## Actual Go comparison

The new explicit offline adapter uses actual `HierarchicalCoordinator.BuildPendingHeaders`, `NodeSet.Update/Entropy` and `HeaderChain.SetCurrentHeader`/rawdb canonical topology. Coordinator pending generation workers are disabled; their behavior is not qualified by this adapter. Go canonical topology application disables state application here: actual Go execution roots come from the separately frozen weak-share `StateProcessor.Process` corpus. The Rust path independently executes each imported block and compares those roots.

[Source-bound native evidence](../../results/canonical-20261113T035507464093Z/summary.json) passes **488 comparisons** over **25 events / 24 distinct native blocks**. A higher-entropy height-15 branch replaces the previous height-16 branch, then extends to height 16. All selected head/height hashes, native frontier scores and EVM/ETX/UTXO roots match the reference. Three reopen checks, pinned old-generation reads, eight Go-invalid candidates, conflicting duplicate context witnesses and fresh catalog corruption checks pass. Duplicate first-arrival behavior is covered; this corpus does not add a distinct equal-entropy fork pair.

Unit tests inject process exits after staging, after catalog publication and after HEAD publication. Recovery chooses the old or complete new generation. Additional tests reject a foreign-store permit, refuse staged-only promotion, recheck objects damaged after admission, enforce the admission byte budget and exercise binary height links across 4,096 synthetic metadata entries. The latter is an index invariant, not a 4,096-block full replay.

## Actual peer acquisition

The reusable optional `private_sync` module fetches backward by requested block hash to a locally admitted ancestor, checks response ID/location/type/hash, scope, native work/order and strict height descent, requests the required higher-context views, then imports forward through native private admission. Acquisition is bounded to 4,095 blocks, 32 MiB of response payloads, one outstanding request, ten-second stream I/O and a sixty-second acquisition deadline. A valid prefix can commit before a later execution-invalid block is rejected; each publication remains atomic. Semantic/correlation failures close the stream.

The real loopback lab uses independent stores and verifier instances for two sources and a receiver, all within **one OS process**. It qualifies **24 imported native blocks, 38 raw-object requests, 61,603 response payload bytes, one canonical reorg and two receiver restarts**. These request/byte counters cover the two successful acquisitions and exclude adversarial probes. Response payload bytes exclude TCP/Noise/Yamux overhead. A wrong hash, wrong request ID, mid-stream disconnect and valid-work/state-invalid body are rejected. The selected final roots and head match the Go oracle. A repeated known head requires zero object requests. No public peer or wallet action occurred. Independent OS-process operation, sustained hostile load and global peer scheduling remain open.

## Build and efficiency evidence

Default and reduced Go-denied builds each pass **194 tests** in the release profile, binding **168 Rust sources / 22 source assets**. The reduced source has no Go checkout and zero Go/protoc attempts. Frozen RPC and two pinned SDKs pass **70/16 comparisons**. [Combined evidence](../../results/canonical-work-20261113T035743722472Z/summary.json) now records **49,144 component comparisons** across overlapping corpora and projections. That is not a unique coverage count or completion percentage.

Binary ancestor links and single-pointer publication remove full height-map rewrites and a second mutable canonical pointer. Byte limits and task draining bound the demonstrated private flow. No paired end-to-end performance experiment was frozen in this increment, so no new client speedup, throughput or RSS reduction is claimed. The peer report retains single-run qualification wall time including restarts; it is not a comparative benchmark. [The performance ledger](../../PERFORMANCE_LEDGER.md) preserves measured gains and regressions under their original source/workload contracts.

## Remaining critical path

1. Extend the composed admitted-block path to mixed signed Quai/Qi and every ETX kind, later auxiliary/fork/economic boundaries, nonlocal/sibling histories and expansion. Isolated component success does not close these integrated domains.
2. Integrate complete multi-zone coordinator eligibility, leader search, pending generation workers and competing frontier histories. The private path uses authenticated native entropy; the older hash-only projection still rejects nonempty pre-fork shares.
3. Replace bounded complete-image storage/recovery with production incremental state, receipt/log indexes, pruning and disk/platform fault qualification.
4. Run independent OS-process private nodes through sustained competing-fork, disconnect, crash and malformed-peer workloads. Qualify an explicit public target before shadow sync.
5. Publish live validated state through RPC only after its full gates pass; then qualify wallet, pool, filters/subscriptions and Linux whole-client CPU/RSS/disk/p99 workloads.

`Verifier.validate_block` still returns Unsupported. The global readiness gates remain partial/open; only the Rust-only build boundary is passed.

## Reproduce

    python3 scripts/build.py
    python3 scripts/verify_canonical.py --peer
    cargo test --release --locked --offline -p quai-consensus --test canonical --lib
    python3 scripts/verify_rust_build.py

Explicit offline Go check: `GOCACHE=/private/tmp/quai-benchmark-go-cache GOPROXY=off python3 scripts/gen_canonical_vectors.py --go-quai ../go-quai --check`. Normal builds and native qualification never invoke Go. Rollback and readable content/metadata inspection are recorded in `results/canonical-installation.json`.
