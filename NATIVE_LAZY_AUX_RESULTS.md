Later source increment: [sealed local admission at transaction activation](NATIVE_ACTIVATION_RESULTS.md). The measurements below remain tied to the earlier source and frozen workloads.

# Sparse ETX, economic metadata and lockup state

Full consensus and public canonical sync remain unfinished. Official go-quai v0.57.0 (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`) remains the pinned oracle. Go stays absent from default build, distribution and runtime. Public JSON-RPC remains frozen, unverified and read-only; global `Verifier.validate_block` remains Unsupported.

## State loading and commits

The private native canonical writer now restores ETX queues, economic metadata and raw lockups through pinned point readers. Accounts/code for requested accounts, storage slots, Qi membership, catalog/frontiers and branch ancestry/hierarchy were already lazy. Native restoration no longer enumerates the complete execution-state maps. It still performs bounded metadata/root checks: queue interval, oldest/newest consensus-index binding, queue-data empty-root coherence and empty consensus/data-root coherence. Untouched leaves are verified when accessed; this is trusted local receipt-based state, not a peer state importer or a full database scrub.

Queue reads check index preimages, canonical external-transaction bytes and the exact envelope in the consensus ETX trie. A missing record inside the live interval is unavailable state; an index outside that interval is absent. Economic metadata checks its preimage/width and consensus-trie value. Lockups verify their 47-byte key, value width and canonical local encoding. Stored roots update from their pinned parents and changed keys. Queue pops/pushes, economic fields, raw lockups, accounts/storage/code and Qi records commit with the named root set and selected-version protocol.

ETX state now shares transaction values and uses copy-on-write mutation maps. Stored bases keep at most 65,536 touched queue keys per overlay, 4,096 touched economic keys, 32 MiB of envelope/metadata payload and 64 MiB of estimated encoded/decoded buffer bytes. Queue cache weights include encoded transaction data and envelopes, with 1,024 entries / 16 MiB of estimated buffers. Lockup overlays cap touched keys at 65,536; the point cache caps entries at 4,096. These are logical/configured limits, not measured retained heap or RSS. Legacy in-memory queue/export limits remain 16,384 entries; stored total membership is not capped by that old limit. Explicit diagnostic/image exports remain bounded and may refuse a larger stored base.

Backend failures propagate through queue operations, incoming execution, lockup calls and rollback, economic authority updates and native root/availability checks. In particular, an unavailable authority metadata write is no longer converted into an ordinary false command result. Unavailable queue data cannot look like an empty queue, and unavailable lockups cannot become default tranches. Existing fork-specific raw lockup rollback behavior remains governed by the frozen Go corpus.

## Compatibility

**Disk encoding is unchanged from the preceding sparse-account source:** canonical format 5, keyed marker 3, branch metadata 5, execution codec 2, catalog schema 1 and hierarchy delta 1. Compatible format-5 private datadirs can be reopened by the current source. Earlier formats are still preserved and refused without migration. A separate actual old/current binary check imports three frozen native blocks using the previous binary, reopens/appends two using the current binary and reopens the resulting selected generation using the previous binary. It checks head, checkpoint, roots and peer identity, within the existing small early private profile. Older materializing binaries are not qualified to read larger new states above their host limits.

The low-level Rust queue read API is fallible and returns shared transaction values. Diagnostic queue/lockup exports are explicit and fallible. Authority metadata updates now return a result. The default public RPC contract, block encodings, private profile and dependency lock are unchanged.

## Evidence

New local storage tests restore a 5,000-ETX / 5,000-lockup parent with zero record decodes, change only specified records, preserve pinned parents across writer reopen and compare complete recovered fixture values. Another real local version reaches 16,385 queued ETXs, above the old total limit, then continues reading and committing while bounded export refuses the larger base. These synthetic versions are not additional admitted blocks.

Fault tests cover missing/malformed queue records inside a live interval, malformed lockup values, missing economic metadata, nested lockup backend failure after an outer storage write, incoming queue failure with unchanged state and all gas/budget counters, and authority metadata failure classification. Synthetic damaged versions and fault readers have no admission authority.

The final default and reduced Go-denied builds each pass **247 release tests / 196 Rust sources / 22 assets**, with zero Go/protoc attempts. Persisted component execution retains **18,312 Go comparisons / 1,371 cases / 2,706 roundtrips**. Native canonical qualification retains **488 checks / 24 distinct blocks**; separate Rust processes retain **165 assertions**. Actual previous/current Rust binaries add **9 same-format compatibility assertions**; format-4 refusal retains **11 assertions**. Frozen RPC/SDK checks remain **70/16**. Source-bound evidence is recorded in `results/latest-lazy-aux-work.json`. The native corpus remains the same 24 distinct early private blocks. Large local state tests and compatibility reopens do not establish full mixed-transaction/later-fork/global network coverage.

## Measurements

The [separate frozen auxiliary-storage contract](configs/benchmark-sparse-aux-v1.json) compares one current Rust binary in materialized and sparse modes, against the same codec-2 layout. Its parent contains 5,000 queued ETXs and 5,000 raw lockups. It pops two entries, pushes one, changes one economic value and changes three lockup keys before an identical durable child commit. Both modes require exact queue order/data/indexes/metadata, lockup values, consensus roots, local digest and version identity.

The timer includes fresh snapshot, restoration, clone, point updates and durable persistence. Database/cache initialization, parent creation/commit, expected-state construction, final full export/checks and cleanup are excluded. Record-load counters precede diagnostic export. Five alternating sequential timing pairs and three separate allocation pairs retain raw samples, source/binary/driver/contract hashes and shared-host load. Gross requested allocation bytes include reallocations, exclude frees and are not retained memory or RSS. This is synthetic storage work, not a signed transaction or native-block throughput result.

The two existing source comparisons retain identical short native branch and canonical reopen harnesses, using the preceding sparse-account source as baseline. Every regression remains in the ledger. No Go speed, cold-disk, public-network or whole-client TPS claim follows; ratios must not be multiplied by earlier measurements.

## Remaining full-client gates

Broaden sealed canonical admission across mixed signed Quai/Qi/all ETX kinds, later economic/auxiliary/fork boundaries, nonlocal/sibling histories and expansion. Finish global coordinator eligibility/search/pending workers and full EVM resource qualification. Qualify sustained state/history scale and reader retention, indexes and pruning, Linux/platform faults, automatic acquisition/head following and malicious/slow peers. Complete explicit public shadow-target and verified live state RPC gates before global validation/public-sync readiness. Independent code fetching and further working-set tuning remain optimization work.

The [performance ledger](PERFORMANCE_LEDGER.md) records exact medians, every regression, raw samples and source/binary/contract hashes.
