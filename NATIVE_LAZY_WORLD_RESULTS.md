Current extension: [sparse ETX, economic metadata and lockups](NATIVE_LAZY_AUX_RESULTS.md). Counts and timings below identify the preceding sparse-account source.

# Sparse Quai account and contract storage state

Full consensus and public canonical sync remain unfinished. This source extends the pinned official go-quai v0.57.0 reference (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`) with native account/storage providers. Go remains an explicit offline oracle and stays absent from the default build, distribution and runtime. The default JSON-RPC remains a frozen, unverified read-only service; global `Verifier.validate_block` remains Unsupported.

## Execution and persistence

The native private canonical writer restores a World from a pinned account root and local count metadata. Startup verifies the root-set digest, named root encodings, account/index empty-root consistency and the existing independent Qi MuHash/count metadata. It does not enumerate account or storage leaves. Account lookup verifies the address preimage, exact local record width, five-field consensus account value, storage-root presence/count coherence and code hash. Code for a requested account loads with that account; code is not yet fetched independently of account metadata. A bounded account cache keeps at most 1,024 entries and 16 MiB of code bytes, excluding container overhead. The database cache stays configured at 64 MiB; neither figure is measured RSS.

Each storage read traverses the requested secure trie path and strictly decodes its nonzero RLP value. Local writes retain a bounded key overlay, including zero tombstones. Storage roots update from the pinned parent trie and those changed slots. Account count and stored live-slot count are independent from Quai's consensus storage-size field. Journal size baselines now capture accounts on first touch, and finalization visits those captured accounts rather than the complete World. Existing CREATE/recreate, nested revert, empty-account cleanup and independent storage-size behavior remain subject to the frozen Go transition corpus.

Native persistence visits touched accounts and membership keys. It writes account/storage/code, ETX/queue/lockup changes, independent Qi metadata and named roots in one transaction, then uses the existing canonical receipt and selected-version protocol. Staged state is not a peer admission claim. Account deletion removes its account/index entries and drops its storage root without walking all old slots.

The private execution codec now retains slot preimages by secure hash of the 32-byte slot key, independently of an account's lifetime. Identical slot keys across accounts share a preimage. Old preimages and code remain available to pinned parents and are not deleted with an account; pruning and retention policy remain unfinished. ETX queues and lockups still materialize bounded maps. Catalogs/frontiers, branch ancestry/hierarchy and Qi membership continue to load on demand.

Stored bases no longer inherit the total in-memory account or per-account slot limits. Working overlays remain bounded: 65,536 touched account keys, 65,536 overlay slots per account, one million aggregate overlay slots and 64 MiB of logical account working bytes. Legacy in-memory fixtures retain their prior limits. Explicit image/diagnostic exports remain bounded and may refuse a valid stored base that is too large to materialize. Aggregate export slots/bytes are checked before each account's storage scan. These logical limits are not a retained-memory or RSS guarantee.

## Failures and disk compatibility

Account and slot reads return a typed distinction between genuine absence and unavailable/corrupt local state. Backend failures propagate through EVM opcodes, nested CALL/CREATE, journal operations, lockup dispatch, signed settlement, incoming wrapping and zone execution/root finalization. They abort validation instead of producing zero storage, an empty account, a successful receipt or an ordinary EVM revert. Snapshot database ownership and the exclusive Unix writer lease retain the preceding close/reopen guarantees. Hash/shape checks apply when a path is used; startup does not scrub every historical leaf. This local receipt-based provider is not a peer state importer.

**A fresh private datadir is required.** Canonical format is now **5**, the keyed marker **3**, branch metadata **5**, hierarchy delta **1**, catalog schema **1** and execution codec **2**. Previous private formats are preserved and refused, with no automatic migration. An old/new binary check creates a real previous-format Rust datadir, verifies the current binary refuses it without changing any file byte hash, and confirms the old binary can still reopen it. That check does not import or migrate old data. Public block encodings and frozen JSON-RPC behavior are unchanged.

The Rust account API now returns a fallible owned account view; storage reads are fallible too. `ReadError` is shared with Qi while the previous Qi re-export remains. The existing sparse-Qi restore entry point is retained as a compatibility alias for sparse account/storage/Qi restoration. Representation equality is not semantic equality across different providers; diagnostic comparisons explicitly export the bounded fixture account/storage values.

## Qualification scope

Targeted tests cover zero-account-load restoration, touched writes/transfer/storage-size finalization, parent snapshots across writer reopen, a real stored **65,537-account** state, and a stored account growing from 4,096 to **4,097 slots** followed by deletion. The old bounded export refuses those larger total states while native reads and persistence continue. These are synthetic local storage versions, not additional mined/admitted blocks.

Fault tests cover malformed/missing account metadata, missing code, nested CALL storage failure after an outer write, nested CREATE account failure after storage/nonce writes, and a real frozen signed Quai transaction with unavailable target metadata. The enclosing input state, relevant gas counters and selected head stay unchanged. Synthetic corrupt versions have no native admission authority. Diagnostic export tests stop before the aggregate storage budget is exceeded. Native staged-genesis recovery tests semantic empty state rather than provider representation.

The final default and reduced Go-denied builds each pass **241 release tests / 192 Rust sources / 22 assets**, with zero Go/protoc attempts. Persisted component execution retains **18,312 Go comparisons / 1,371 cases / 2,706 roundtrips**. Native canonical qualification retains **488 checks / 24 distinct blocks**; separate Rust processes retain **165 assertions**. Actual old/new Rust binaries add **11 old-format preservation/refusal assertions**. Frozen RPC/SDK checks remain **70/16**. Source-bound evidence is recorded in `results/latest-lazy-world-work.json`. The fixed native corpus remains the early private profile with 24 distinct blocks; the wider signed Quai/Qi/incoming transition corpus is component execution, not a claim of complete mixed-transaction canonical admission.

## Measurement contract

The new [frozen synthetic contract](configs/benchmark-sparse-world-v1.json) compares one current Rust binary in explicit materialized and sparse modes against the same codec-2 database. Its parent has **10,000 accounts / 20,000 contract slots**. A journal changes two slots and transfers a balance across three accounts, then block storage-size finalization and durable persistence produce an identical child. Every mode checks exact account/storage values, consensus roots, local root-set digest and version ID.

The timer includes fresh snapshot creation, parent restoration, clone, journal updates, storage-size finalization and durable commit. Database/cache initialization, parent creation/commit, expected-result construction, final full export/checks and cleanup are excluded. Five alternating sequential timing pairs and three separate allocation pairs retain source/binary/driver/contract hashes, raw samples and shared-host load. The account-load counter is captured before final diagnostic export. This is synthetic storage/journal work, not a signed-transaction or native-block throughput benchmark.

Separate source comparisons retain the unchanged sixteen-block hierarchy/coinbase harness and unchanged 25-event/24-block canonical reopen harness, using the preceding pinned-state source as baseline. They include the private layout change as well as lazy providers. All regressions stay in the performance ledger. Gross requested allocations include reallocations and are not retained heap or RSS. No Go speed, cold-disk, real-network or whole-node TPS claim follows, and ratios must not be multiplied with earlier measurements.

## Remaining full-client work

Broaden sealed canonical admission to mixed signed Quai/Qi/all ETX kinds, later fork/economic boundaries, nonlocal/sibling histories and expansion. Complete global coordinator eligibility/search/pending workers and qualify full EVM resource behavior. Replace ETX/lockup materialization, add receipt/log indexes and safe pruning, and test sustained scale plus Linux/platform faults. Extend private acquisition to automatic broad-history following and malicious/slow peers, then qualify an explicit public shadow target and verified live state RPC. Keep global validation and public-sync completion false until those integrated gates pass.

## Reproduction

    python3 scripts/build.py
    python3 scripts/verify_rust_build.py
    python3 scripts/verify_persistent_execution.py
    python3 scripts/verify_canonical.py --peer
    python3 scripts/verify_private_processes.py
    python3 scripts/build_private_node.py
    python3 scripts/verify_private_storage_upgrade.py --baseline /path/to/qualified-previous-source
    python3 scripts/benchmark_sparse_world.py

These checks require no Go runtime, production peer or wallet. The separate old-format check requires its previously qualified Rust private-node executable.

The [performance ledger](PERFORMANCE_LEDGER.md) records exact medians, every regression, raw sample links and source/binary/contract hashes for this source.
