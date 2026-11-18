# Performance and efficiency ledger

Only measured, source-bound results belong in this ledger. Each entry preserves the workload, reference, correctness evidence, raw samples and source/binary hashes. Timing, allocated bytes, retained memory/RSS, disk behavior and network throughput are separate measurements. No entry establishes full consensus, canonical sync or production readiness.

## 2026-11-11: shared state and cached commitments

Official go-quai v0.57.0 reference `33db82f2891b267ba898ca38bd2dd8bf75cbb072`. Five alternating paired rounds compared the prior Rust implementation with the shared-state/cached-commitment implementation on the same offline workloads. Values below are median milliseconds per operation.

| Workload | Prior Rust | Optimized Rust | Speedup |
| --- | ---: | ---: | ---: |
| VM SSTORE call, 4096 accounts | 2.332956 | 1.013749 | 2.30x |
| One changed account plus account-state root | 61.959265 | 10.387543 | 5.96x |
| Signed Quai transfer | 0.541687 | 0.152175 | 3.56x |
| Signed Quai storage transaction | 0.489546 | 0.173612 | 2.82x |
| Signed Quai creation | 0.565424 | 0.266229 | 2.12x |
| Shared account snapshot clone/drop | 0.586367 | 0.000041 | 14301.63x |
| Unchanged account-root query | 55.992716 | 0.011466 | 4883.37x |

The account snapshot and unchanged-root queries reuse shared/cached data. The changed-account workload includes an actual state change, VM execution and root computation. These results are offline library measurements on a shared host. They exclude input parsing and state setup; they do not measure network sync, node TPS, disk commits or retained memory.

- [Raw samples, source/binary hashes and measurement contract](results/consensus-performance-20261111T153319572894Z/summary.json)
- [Complete baseline source and fixtures](results/consensus-performance-20261111T153319572894Z/baseline-source.tar.gz)
- [Correctness and source-build evidence](NATIVE_CONSENSUS_RESULTS.md)

The earlier pre-cache run showed a noisy 13% root-recomputation slowdown. Its [raw results and exact source](results/consensus-performance-20261111T151454965300Z/summary.json) remain available. The subsequent cache change passed the differential corpus and a separate fixed changed-account/root workload.

Allocation and RSS: no new numbers were measured in this round. Account and UTXO snapshots share backing data; derived caches consume additional retained memory. Do not infer a peak-memory reduction from timing or source inspection.

## 2026-11-11: bounded inclusion-work memo

Five alternating sequential paired rounds compare the same new reward-distribution code before and after positive work memoization. The uncached baseline source is archived; the corpus, compiler, release profile and benchmark harness match. Each sample performs 400 reward distributions on the same verified private history after warming the native epoch. Every sample has the same outbound root, reward count and work-request count.

| Fixed offline workload | Uncached Rust median (ms/op) | Memoized Rust median (ms/op) | Speedup |
| --- | ---: | ---: | ---: |
| Repeated reward distribution on identical history | 19.637059 | 0.074347 | 264.13x |

This is a cache-hit measurement. It avoids repeated native proof calculations for the same header; it does not establish a speedup for first-seen headers, full block replay, canonical sync, node TPS or RPC. Parsing, epoch construction and output-root checking are outside the timer. The shared-host raw samples include load averages.

- [Raw samples, workload/output contract and source/binary hashes](results/payout-performance-20261111T172411642184Z/summary.json)
- [Exact uncached source and fixtures](results/payout-performance-20261111T172411642184Z/baseline-source.tar.gz)
Baseline correctness logs are included in the source archive. [Candidate block/payout checks](results/zone-execution-20261111T171815650845Z/summary.json) are retained separately.

The memo retains only successful inclusion-work checks, keyed by full normalized header bytes and node scope. It is bounded to 512 entries and 4 MiB of logical keys/entries per verifier; allocator/container overhead and transient encoding allocations are additional. Those are implementation limits, not measured retained memory or RSS. No new allocation/RSS savings were measured. Block proof target verification remains independent. Tests cover mutation/scope misses, rejected work, resource limits and refusal to treat a weak cached share as a valid block proof.

The paired uncached/cached sources both pass the 486 payout projections and 16 linked native-work private blocks. That source increment passed 173 default tests, 40,511 component comparisons, 112 atomic forged-execution rejections and 16 complete local state-image restores. Broader historical/fork integration and full consensus/canonical durability/sync remain incomplete. See [current results](NATIVE_CONSENSUS_RESULTS.md) and [the remaining gates](NATIVE_CONSENSUS_PROGRESS.md).

## 2026-11-11: private journal head switches

A fixed 64-checkpoint, two-branch journal workload now measures the ancestry traversal change. Each branch shares its first 48 checkpoints; opaque payloads are 256 KiB. Every switch still verifies complete object checksums and fsyncs/renames HEAD. Metadata is reused only within that operation; no integrity result is cached across calls. Both sources produce identical branch plans and final selected-head identifiers.

| Metric per switch | Previous Rust median | Optimized Rust median | Improvement |
| --- | ---: | ---: | ---: |
| Warm-filesystem latency | 184.918531 ms | 79.460948 ms | 2.33x |
| Gross allocated bytes | 272,838,480 | 83,989,872 | 69.2% fewer |
| Allocation requests | 11,482 | 4,374 | 61.9% fewer |

Five alternating paired timing rounds and three separate allocation rounds run sequentially. Allocation measurements count gross Rust requests, including the full size of reallocations; they are not retained/live heap or peak RSS. Timings include checksum/traversal and HEAD fsync/rename on a shared host with a warm filesystem cache. They do not establish full-wrapper recovery/commit latency, cold-disk speed, pruning cost, block TPS or network sync speed.

- [Raw measurements, fixed workload and source/binary hashes](results/journal-performance-20261111T213425497812Z/summary.json)
- [Complete pre-optimization source and correctness logs](results/journal-performance-20261111T213425497812Z/baseline-source.tar.gz)
- [Private branch recovery and correctness scope](NATIVE_DURABLE_RESULTS.md)

The change removes duplicate ancestry reads/checks and payload cloning for metadata lookup. Tests preserve corruption detection on subsequent calls and reject unrelated genesis/stale heads. The source increment also adds durable execution-only branch images/receipts/history, qualified on two actual-Go private branches; those functional additions are not a measured end-to-end speedup.

## 2026-11-12: sequential native header preparation

A separate frozen corpus now qualifies local-zone header/placement plus execution against real Go validation, preserving the older execution-only profile. The performance workload processes sixteen distinct headers once in order, including native header/body/manifest/PCRC components and history/index insertion. Decoding and native epoch construction are outside timing.

| Fixed workload | Previous Rust median (ms/header) | Optimized Rust median (ms/header) | Speedup |
| --- | ---: | ---: | ---: |
| Sequential local-zone header preparation | 16.660309 | 6.556700 | 2.54x |

Five alternating sequential paired rounds use the identical harness, profile and corpus. Final header/manifests and native work hashes match. Previously authenticated parents and repeated checks within preparation reuse successful full-proof results. Policy and clock checks still run. This is a sequence of distinct candidates; it is separate from the earlier repeated-reward cache-hit workload.

- [Raw timing, workload/output contract and source/binary hashes](results/admission-performance-20261112T155728377287Z/summary.json)
- [Exact uncached baseline source and correctness logs](results/admission-performance-20261112T155728377287Z/baseline-source.tar.gz)
- [Native header/execution correctness and scope](NATIVE_ADMISSION_RESULTS.md)

The full-proof memo has a 512-entry/4 MiB logical limit per verifier and is separate from the weaker inclusion-work memo. Failures and inclusion-only results never populate it. Memory overhead is additional; no new allocation/RSS saving was measured. Timings exclude execution, durable commits, RPC and network sync. Shared-host noise is retained in the raw samples. These figures do not qualify mainnet or complete canonical synchronization.

## 2026-11-12: mixed-order hierarchy execution and durable commits

A fixed sixteen-block private sequence now compares the same new confirmation code before and after reusing the exact last checked hierarchy prefix. The sequence has 25 native prime/region/zone context appends and eight derived incoming coinbases. Both versions produce identical final block and ETX commitments. Full proof and reward-inclusion work is warmed before timing; fixture decoding and genesis setup are excluded. Current policy/rollup checks, execution/images, fresh ancestry checksums, staging and selected-head fsync/rename are timed.

| Metric per block | Previous Rust median | Cached-prefix Rust median | Improvement |
| --- | ---: | ---: | ---: |
| Warm-work execution and durable commit | 28.183757 ms | 23.939349 ms | 1.18x |
| Gross allocated bytes | 3,772,498 | 1,836,551 | 51.3% fewer |
| Allocation requests | 45,203 | 16,245 | 64.1% fewer |

Five alternating sequential timing pairs and three separate allocation pairs run on a shared host with a warm filesystem cache. Gross allocations include full requested reallocations; they are not retained heap/RSS. The cache retains an additional bounded graph, so no RAM/RSS reduction is claimed. Candidate policy and clock checks rerun; every append checks fresh journal ancestry checksums, and a populated prefix cannot mask corrupt stored bytes. No Go comparison, cold-proof/disk result, full-node TPS, RPC or network-sync speedup is established.

- [Raw samples, measurement contract and source/binary hashes](results/confirmation-performance-20261112T191430267556Z/summary.json)
- [Complete pre-cache source and passing baseline test](results/confirmation-performance-20261112T191430267556Z/baseline-source.tar.gz)
- [Three-context/rollup/execution correctness and remaining gates](NATIVE_CONFIRMATION_RESULTS.md)

The functional increment qualifies two early private branches, 24 distinct blocks, 38 context bundles and 430 comparisons against actual Go methods. General nonlocal/mixed/fork/workshare/expansion and sealed full admission, canonical publication and sync remain unfinished. The current source also passes 184 tests and 41,514 combined component comparisons; those overlapping comparisons are not a completion percentage.

## 2026-11-12: one parent restore per hierarchy append

A new frozen early private corpus integrates real full-target ProgPoW uncles, integer entropy, rewards and derived incoming coinbases. The paired optimization removes a duplicate parent-state restore within each hierarchy append. Both versions retain fresh ancestry checksums per call and the journal's immediate-parent stage check. No new cross-call state/integrity cache is introduced.

| Metric per block | Duplicate restore baseline | Single restore | Change |
| --- | ---: | ---: | ---: |
| Warm-proof execution and durable commit | 24.549523 ms | 23.625825 ms | 1.04x |
| Gross allocated bytes | 2,075,261 | 1,655,669 | 20.2% fewer |
| Allocation requests | 17,864 | 14,104 | 21.0% fewer |

Five alternating timing pairs and three separate allocation pairs run sequentially. Each process executes sixteen distinct blocks with 25 native context appends, eight full-target uncle inclusions and twelve derived incoming coinbases. Block/uncle and reward proofs are warmed outside timing, as are fixture decode/genesis setup. The timer includes current policy/confirmation, execution/images, checksum scans, durable staging and HEAD fsync/rename on a warm filesystem cache. Outputs match in both versions.

Allocation metrics count gross Rust requests, including full new reallocations; retained heap and peak RSS remain unmeasured. The observed median latency reduction is 3.8%, with overlapping sample ranges; treat this as modest timing evidence. Shared-host load and raw samples are retained. These figures are not a cold-work/disk, Go comparison, full-consensus, node-TPS, RPC or sync result, and cannot be multiplied by earlier results from different corpora.

- [Raw measurements and source/binary hashes](results/uncles-performance-20261112T200634720729Z/summary.json)
- [Complete pre-optimization source and passing baseline test](results/uncles-performance-20261112T200634720729Z/baseline-source.tar.gz)
- [Uncle/entropy/reward compatibility and remaining limits](NATIVE_UNCLES_RESULTS.md)

The functional increment adds 505 comparisons on 24 blocks/38 context bundles, including a Go-valid weak-share capability boundary. The current source passes 185 tests and 42,019 combined component comparisons. General admission, weak historical shares and canonical publication/sync remain unfinished.

## 2026-11-12: exact weak-share arithmetic workspace

The native weak-share calculation now reproduces the pinned Go cubic, coefficient bits, precision propagation, nearest-even rounding and truncation. A fixed corpus contains 5,875 Go-qualified deficit/distance inputs. The comparison changes only the arithmetic workspace from 512 to 256 bits. Both sources pass native branch and arithmetic checks; corpus, algorithm, compiler profile, lockfile and harness match.

| Fixed arithmetic kernel | 512-bit workspace | 256-bit workspace | Observed speedup |
| --- | ---: | ---: | ---: |
| Median per discount | 20.399 us | 4.963 us | 4.11x |

Five alternating timing pairs run sequentially. Each process performs an untimed per-input check, then 94,000 discounts. Exact checksums match; parsing/setup are excluded. Three separate allocation pairs observe zero Rust heap requests in both kernels, so no allocation reduction is claimed. Raw shared-host load and samples are retained. Retained memory/RSS are unmeasured.

This is arithmetic-kernel evidence, not Go-versus-Rust, full block/proof/storage, RPC, node TPS or network sync performance. Earlier gains from different workloads are not cumulative speedups.

- [Raw samples, source/binary hashes and workload](results/weakshares-performance-20261112T203851030678Z/summary.json)
- [Complete 512-bit baseline and passing source-bound validation](results/weakshares-performance-20261112T203851030678Z/baseline-source.tar.gz)
- [Weak-share, native score and branch compatibility](NATIVE_WEAKSHARES_RESULTS.md)

The functional increment adds 760 native and 5,877 arithmetic checks. It qualifies 10 weak shares, 13 full-target uncles, 30 derived incoming coinbases and 15 local native scores across 24 blocks/38 context bundles. The current source passes 187 tests and 48,656 combined component comparisons. Broader histories and full consensus/canonical synchronization remain unfinished.

## 2026-11-12: private canonical publication and peer acquisition

The fixed early private profile now composes native admission, frontier selection, atomic selected HEAD and immutable generation reads. Binary ancestor links replace full height-map rewrites with O(log height) lookup; a single selected HEAD avoids a second mutable canonical pointer. Admission/catalog and acquisition byte limits are enforced. Production incremental state/pruning, global consensus and public sync remain open.

The source-bound functional run passes 488 Go comparisons over 25 events/24 blocks. The actual loopback run imports 24 blocks in 38 requests (61,603 response payload bytes), performs one higher-entropy reorg and survives two receiver restarts; four bad-peer/block cases are rejected. Sources and receiver use independent stores in one OS process. All final commitments match the offline oracle.

**No new paired speedup or RSS measurement was performed.** The peer report's single-run wall time includes imports and restart checks and is functional qualification latency. Structural complexity improvements are not promoted to measured node throughput. Earlier gains below retain their original contracts and cannot be multiplied.

- [Native canonical and peer evidence](results/canonical-20261113T035507464093Z/summary.json)
- [Implementation, limits and open gates](NATIVE_CANONICAL_RESULTS.md)
- [Current complete build and overlapping component checks](results/canonical-work-20261113T035743722472Z/summary.json)

## 2026-11-13 UTC: independent private Rust nodes and local known-head acquisition

The fixed early-profile canonical path now runs in independent OS processes with persistent checksummed peer identities, native raw-object acquisition/selection, receiver/source restarts, SIGKILL recovery, bounded local control and cancellation between atomic block commits. The dedicated console reader also resolves a process-shutdown hang caught by the independent-node test.

A known locally admitted head now returns without dialing or opening a request stream. Library qualification uses an unenrolled peer; process qualification succeeds while the source is offline. This removes unnecessary network work and preserves the selected generation. It is a structural/functional result, not a measured throughput ratio.

The source-bound primary run imports 24 native blocks with 38 requests/61,603 response payload bytes and one native reorg. Its 117 assertions include exact Go commitments, two receiver restarts, one source restart, kill/recovery/resume, cancellation/resume and identity/control bounds. Additional drills are excluded from those primary traffic counters. Nine feature-library tests and 194 default/Go-denied tests pass.

No paired client speedup, allocation or RSS experiment was performed. Earlier arithmetic/VM/state/journal gains below remain historical measurements with their own workloads and cannot be multiplied. Full global consensus, automatic production sync and production storage remain open.

- [Independent process evidence](results/private-processes-20261113T050448168865Z/summary.json)
- [Runtime, limits and optional bundle](NATIVE_PROCESS_RESULTS.md)
- [Complete source-bound work summary](results/process-work-20261113T051026074109Z/summary.json)

## 2026-11-13: incremental account roots and shared snapshot commitments

Five alternating paired rounds compare the prior World root algorithm with the incremental algorithm. All other Rust sources, dependencies and each workload are identical. The baseline and candidate use separate Cargo target directories; each exported executable has a different verified hash. The driver is frozen and copied into the result. Both sources pass execution correctness; the current source also passes the full native differential/recovery and Go-denied builds before timing. Timings run sequentially after compilation/tests stop, on a shared host.

| Fixed offline workload | Prior algorithm median (ms/op) | Incremental algorithm median (ms/op) | Speedup |
| --- | ---: | ---: | ---: |
| One SSTORE account change plus new account root | 9.754695 | 0.901550 | 10.82x |
| First-ever root, fresh 4,096-account state | 54.993249 | 56.563725 | 0.97x |
| SSTORE call, 4,096 accounts | 1.132427 | 0.841279 | 1.35x |
| Signed Quai transfer | 0.181391 | 0.163704 | 1.11x |
| Signed Quai storage transaction | 0.186925 | 0.165301 | 1.13x |
| Signed Quai creation | 0.270738 | 0.290720 | 0.93x |

The changed-account workload includes actual SSTORE execution and the resulting state root. The initial-root workload constructs fresh independent state before each sample and times its first root only. Setup, parsing and output checks are excluded. Every run must match the fixed roots, gas and checksums. Fresh-root construction is about 2.9% slower and signed creation about 7.4% slower in these medians; transfer/storage improve and other sample noise is preserved. These figures do not establish whole-client, network-sync or Go speedups, and must not be multiplied by earlier ledger ratios.

Shared snapshot clone/drop medians are 46 / 26 ns (prior/new). Unchanged root-query medians are 15595 / 54 ns. The original clone-root guard reports 8335045 / 58 ns. That guard clones the same parent after a verification call and can reuse parent-shared encodings/commitments; it is a cache-reuse workload, not first-ever-root construction. Nanosecond microbenchmarks are sensitive to timer/optimizer/shared-host effects; raw samples and their exact harnesses are preserved.

The initial qualified path-copying experiment improved changed-root cost 11.47x but regressed the clone-root guard from 7.374 ms to 56.041 ms. Initial snapshot construction now uses a sorted bulk builder, and unchanged clones share the derived root until a write detaches it. A new independent-state initial-root workload checks the actual cold path. The [earlier valid experiment](results/incremental-state-performance-20261113T065210705273Z/summary.json) remains available with its original source, binaries and workload; its clone guard was initially labeled cold and should be interpreted with the sharing boundary above.

A [rejected first run](results/incremental-state-performance-20261113T064825999707Z/superseded.json) copied the same cached executable into both roles and changed its driver after launch. It supports no speed claim. Original outputs are preserved and its summary is marked unqualified. The accepted final run fixes both provenance issues.

- [Final raw samples, source/binary hashes and exact measurement contract](results/incremental-state-performance-20261113T071555851404Z/summary.json)
- [Complete controlled baseline source and fixtures](results/incremental-state-performance-20261113T071555851404Z/baseline-source.tar.gz)
- [Frozen benchmark driver](results/incremental-state-performance-20261113T071555851404Z/benchmark-driver.py)
- [State/database correctness and remaining integration](NATIVE_STATE_DB_RESULTS.md)

The separate state database test inserts 95,792 unique trie nodes / 6,998,875 encoded bytes for 70,000 keys, then 341 unique nodes / 100,141 encoded bytes for 100 changes. These are inserted logical records, not physical I/O, disk-write reduction, allocations or RSS. Shared trie nodes and encoded caches retain additional data. No new allocation/RSS result is claimed. Canonical execution still uses bounded images/history; the new backend is not yet integrated into the actual canonical state provider.

## 2026-11-13: keyed execution state in native canonical storage

The native private canonical node now uses keyed transactional execution state. A separate fixed configuration comparison runs one current Rust binary with explicit `images` and `database` constructor arguments. Both modes execute the same sixteen-block hierarchy/coinbase sequence and warm native proof verifier, then restore and check the final block/EVM/ETX commitments. This deliberately uses the same executable for two different checked storage configurations; it is separate from the earlier rejected same-executable source comparison.

Five alternating sequential timing pairs and three separate allocation pairs run after full correctness/Go-denied/peer checks. Values below are medians per block.

| Metric | Complete-image mode | Keyed-database mode | Change |
| --- | ---: | ---: | ---: |
| Warm-work execution, parent restore and durable commit | 22.415099 ms | 11.542617 ms | 1.94x faster |
| Gross requested allocation bytes | 1,457,525 | 1,167,694 | 19.9% fewer |
| Allocation requests | 12,909 | 13,227 | 2.46% more |

The keyed mode avoids complete state-image writes and repeated ancestral image loads. It still materializes bounded execution maps and ancestry. The measured timer includes execution, fresh parent restoration, staging and durable execution-head selection. Parsing, genesis setup, initial database/cache allocation, native proof warming and final checking are excluded. Native canonical catalog publication is outside this branch benchmark. No whole-node, public-sync, Go or physical-disk speedup is established; do not multiply these figures by earlier root or cache ratios.

The **2.46% increase in allocation requests** is retained as a regression. Gross requested bytes include full requested reallocations; they are not live heap or RSS. The database's configured **64 MiB cache** and other startup allocation are outside these per-block counters and can increase retained memory. No RSS reduction is claimed. Raw samples include host load averages; this is shared-host evidence with warm work/filesystem state.

- [Raw samples, exact configuration/workload and source/binary hashes](results/state-storage-performance-20261113T082724557472Z/summary.json)
- [Frozen common source and protocol fixtures](results/state-storage-performance-20261113T082724557472Z/source-and-fixtures.tar.gz)
- [Executed frozen driver](results/state-storage-performance-20261113T082724557472Z/benchmark-driver.py)
- [Native canonical storage qualification and remaining scope](NATIVE_CANONICAL_DB_RESULTS.md)

The functional increment passes 214 default/Go-denied tests, the unchanged 488 native canonical Go comparisons over 24 blocks, and 133 separate-process assertions. Persisted signed Quai/Qi/incoming components add a separate 1,371-case / 18,312-comparison replay of existing Go expectations; these comparisons overlap the original component corpora and are not a completion percentage. Full global consensus, unbounded lazy state/history, production indexes/pruning and public canonical sync remain unfinished.

## 2026-11-13: bounded ancestry readers and indexed canonical recovery

The native private client now loads branch ancestry and hierarchy output on demand, retains a bounded recent overlay, shares decoded immutable profile/version data and uses indexed native admission receipts/frontiers. It no longer rebuilds the complete canonical entry graph on reopen. Full global consensus and public sync remain unfinished.

The first ancestry source variant regressed on the fixed short branch workload: **11.486984 → 12.922192 ms/block (+12.49%)**, **1,167,694 → 2,366,638 gross requested bytes (+102.68%)**, and **13,227 → 29,613 allocation requests (+123.88%)**. Its full raw run and both frozen source/binary artifacts remain in [the initial experiment](results/indexed-history-performance-20261113T094044900457Z/summary.json). That source was superseded before installation. Repeated profile/genesis construction, disposable hierarchy setup, repeated version-header decoding and per-block hierarchy rebasing accounted for avoidable work. Untimed exploratory allocation reports remain explicitly provisional; the table below uses the subsequently qualified source comparisons.

The existing branch comparison retains the unchanged sixteen-block hierarchy/coinbase harness and keyed database configuration. Seven alternating sequential timing pairs and three separate allocation pairs run after correctness checks. The new canonical recovery comparison adds the identical common harness to a temporary copy of the frozen prior source and to the current source. Both import the exact 25-event/24-block native history, assert Go selection/head expectations, then drop their writers and views. Five alternating sequential timing pairs and three separate allocation pairs measure a fresh writer reopen on the warm filesystem, followed by exact head/height/score/root checks.

| Metric | Previous qualified Rust source | Combined indexed source | Change |
| --- | ---: | ---: | ---: |
| Branch execution, parent restore and durable commit | 10.796062 ms/block | 11.162224 ms/block | +3.39% |
| Branch gross requested bytes | 1,167,694 bytes/block | 1,058,562 bytes/block | -9.35% |
| Branch allocation requests | 13,227 requests/block | 11,540 requests/block | -12.75% |
| Fresh writer canonical reopen | 729.096083 ms/open | 21.841750 ms/open | -97.00% |
| Reopen gross requested bytes | 96,836,851 bytes/open | 3,390,266 bytes/open | -96.50% |
| Reopen allocation requests | 1,192,736 requests/open | 10,569 requests/open | -99.11% |

The short branch workload remains **3.39% slower** on this host even after reducing allocation work. This retained regression pays for the new persistence/reader path on the finite early workload; no across-the-board speedup is claimed. The reopen improvement is measured separately and does not cancel or multiply the branch result.

Negative time/byte/request changes mean less measured work. Values are medians; raw samples retain host load. Timed roles run sequentially on this shared host. Every source role is compiled in a separate Cargo target, and exact executed binaries, drivers, source archives, fixture hashes and build metadata are retained. These are Rust implementation comparisons, not Go-versus-Rust measurements. Do not multiply their ratios by historical root/kernel results.

The branch timer excludes parsing, genesis setup, initial database/cache allocation, proof warming and final checks; it includes native execution, fresh parent restoration, branch staging and durable execution-head selection. It excludes canonical catalog publication. The reopen timer includes database/cache initialization and recovery, while parsing, initial native imports and final checks are excluded. Immutable profile initialization remains warm from setup; reopened verifier state is fresh. Recovery trusts already admitted local receipts and checks their stored bindings on access, rather than redoing historical native admission on every open. The catalog is not a peer-imported trust source.

Gross requested bytes include full requested reallocations. They are not retained memory or RSS. The catalog adds a configured **16 MiB** cache to the execution database's **64 MiB** cache; startup costs are included in the reopen counters and excluded from the branch counters. Bounded overlays and immutable reader snapshots also retain memory while alive. No net RAM, cold-disk, long public-history, network throughput, whole-node TPS or Linux qualification is established here. Generic 6,001-receipt storage testing is separate from the qualified 24-block native history.

- [Branch raw samples and source/binary/driver manifests](results/indexed-history-performance-20261113T103734716256Z/summary.json)
- [Canonical reopen raw samples and source/binary/driver manifests](results/canonical-reopen-performance-20261113T103840834366Z/summary.json)
- [Storage contract, qualification and remaining scope](NATIVE_INDEXED_CATALOG_RESULTS.md)

## 2026-11-13: native Qi membership on demand

The native private canonical path now restores Qi membership through a pinned trie reader and retains only touched changes for persistence. Actual signed Quai/Qi/incoming component transitions still pass the same 18,312 frozen Go comparisons across 1,371 cases. Global full consensus/public sync and lazy World/ETX state remain unfinished.

The new configuration comparison runs one current Rust binary with explicit materialized/sparse modes. Both start from the same synthetic 50,000-UTXO parent, remove four points, insert four and durably commit an identical child. Five alternating sequential timing pairs and three separate allocation pairs check exact membership, MuHash/count, local digest and version identity. This is storage work, not a signed transaction or native block benchmark.

| Synthetic storage metric | Materialized Qi | Sparse Qi | Change |
| --- | ---: | ---: | ---: |
| Restore + eight updates + durable commit | 139.130000 ms | 30.204750 ms | -78.29% |
| Gross requested allocation bytes | 142,353,914 bytes | 10,407,467 bytes | -92.69% |
| Allocation requests | 924,163 requests | 19,425 requests | -97.90% |

The timer includes snapshot creation, restoration, cloning, membership updates and transactional persistence. Initial database/cache setup, parent generation/commit, expected-result construction, final full export/checks and cleanup are excluded. The sparse path decodes only requested present records; its point counter does not describe materialized full scans. The synthetic data is not a mined or admitted network history. The 64 MiB execution-database cache is configured, not measured RSS. Gross requested bytes include reallocations, exclude frees and do not measure retained heap or resident memory.

The separate before/after source comparison retains the unchanged sixteen-block hierarchy/coinbase workload and keyed backend. Its previous source is the installed indexed-catalog build. Seven alternating sequential timing pairs and three separate allocation pairs measure native execution with warm proof work, fresh parent restoration, staging and durable branch-head selection. Parsing, genesis/database/cache setup, proof warming and final checks remain excluded; canonical catalog publication and network acquisition are outside this workload.

| Short native branch metric | Previous indexed source | Sparse-Qi source | Change |
| --- | ---: | ---: | ---: |
| Execution, restore and durable branch commit | 11.168177 ms/block | 11.233447 ms/block | +0.58% |
| Gross requested allocation bytes | 1,058,562 bytes/block | 1,058,882 bytes/block | +0.03% |
| Allocation requests | 11,540 requests/block | 11,544 requests/block | +0.03% |

Negative time/byte/request changes mean less measured work. Every result is retained, including any short-workload regression. Timed workloads run sequentially on this shared host; raw samples retain load averages. Allocations run separately. Do not multiply these ratios by the earlier private canonical reopen or kernel ratios. No Go performance, full-node TPS, cold-disk, public sync or RSS claim follows from either comparison.

- [Sparse storage raw samples and source/binary/driver manifests](results/sparse-qi-performance-20261113T112646972474Z/summary.json)
- [Short native workload raw samples and both frozen source builds](results/indexed-history-performance-20261113T112713294344Z/summary.json)
- [Compatibility, error behavior and remaining scope](NATIVE_LAZY_QI_RESULTS.md)

The default and reduced Go-denied builds each pass 227 release tests over 188 Rust sources / 22 source assets. Separate Rust processes retain 165 assertions; RPC/SDK checks remain 70/16. The generic 5,000-UTXO storage test and synthetic 50,000-UTXO measurement do not enlarge the 24-block native canonical corpus.

## 2026-11-13: pinned execution-state ownership

This source fixes `Database has been closed` for execution-state snapshots that outlive their writer. Snapshots retain the database; a separate Unix inode lease permits reopen while refusing another live writer. No disk encoding or benchmark workload changed. The immediately preceding qualified sparse-Qi source is the baseline in both source comparisons below. This is a correctness prerequisite for sparse account/storage providers, not an assumed speedup.

### Short native sixteen-block branch

| Metric | Previous sparse-Qi source | Pinned-database source | Change |
| --- | ---: | ---: | ---: |
| Elapsed time (ms/block) | 10.916263 | 11.172020 | +2.34% |
| Gross requested allocation bytes/block | 1,058,882.000000 | 1,058,914.000000 | +0.00% |
| Allocation requests/block | 11,544.000000 | 11,544.000000 | +0.00% |

[7 alternating sequential timing pairs, three separate allocation pairs, raw samples and source/binary/driver hashes](results/indexed-history-performance-20261113T115406039451Z/summary.json). The identical `bench-state-storage.rs` runs against two separately built immutable binaries.

### Fresh-writer canonical reopen after 25 events / 24 native blocks

| Metric | Previous sparse-Qi source | Pinned-database source | Change |
| --- | ---: | ---: | ---: |
| Elapsed time (ms/reopen) | 22.691042 | 22.624875 | -0.29% |
| Gross requested allocation bytes/reopen | 3,390,586.000000 | 3,337,952.000000 | -1.55% |
| Allocation requests/reopen | 10,573.000000 | 10,502.000000 | -0.67% |

[5 alternating sequential timing pairs, three separate allocation pairs, raw samples and source/binary/driver hashes](results/canonical-reopen-performance-20261113T115511619819Z/summary.json). The identical `bench-canonical-reopen.rs` runs against two separately built immutable binaries.

The short branch timer includes native execution with warm proof work, fresh parent restoration, branch staging and durable execution-head selection. Parsing, genesis/database/cache setup, proof warming and final checks are excluded. Canonical catalog publication and network acquisition are outside this timer. The canonical reopen timer includes fresh database/cache initialization and indexed receipt/frontier recovery after a fixed native history; history construction, import/proof work and final checks are excluded. It uses trusted local admission receipts and does not revalidate all history. No snapshot is deliberately kept alive across that timed writer reopen.

Every change is retained, including regressions. Timings are sequential on a shared host with load averages retained. Allocation measurements run separately, include reallocations, exclude frees and are not retained heap or RSS. Configured execution cache capacity remains 64 MiB. The earlier sparse-Qi 50,000-UTXO configuration measurement is historical; it was not rerun and is not a fresh measurement of this source. These ratios do not establish Go performance, full-node TPS, cold-disk or public sync speed, and must not be multiplied with earlier ratios.

The default and reduced Go-denied builds each pass 232 release tests over 188 Rust sources / 22 assets. Frozen persisted execution retains 18,312 Go comparisons over 1,371 cases, native canonical qualification retains 488 checks / 24 distinct blocks, and separate Rust processes retain 165 assertions. These are bounded compatibility checks rather than a whole-client coverage percentage. Full consensus/public sync remain unfinished; account/storage/code and ETX/lockup materialization remain open. [Correctness contract and the next account provider gate](NATIVE_PINNED_STATE_RESULTS.md).

## 2026-11-13: native account and contract-storage reads on demand

The private canonical writer now restores pinned account/storage/Qi providers and persists touched records. Journal size accounting visits touched accounts. Retained global slot preimages permit deletion without walking old slots. Private disk formats change to canonical 5 / keyed marker 3 / branch metadata 5 / execution codec 2; older datadirs are preserved and refused without migration. Full consensus/public sync, lazy ETX/lockups, complete EVM/resource behavior and production scale/pruning remain unfinished.

### Same-source synthetic account/storage configurations

One current Rust binary uses explicit materialized and sparse modes against the same codec-2 database. A fixed 10,000-account / 20,000-slot parent receives two storage writes and a balance transfer across three touched accounts, then block storage-size finalization and a durable child commit. Exact final account/storage values, consensus roots, local digest and version ID must agree.

| Metric | Materialized account/storage state | Sparse account/storage state | Change |
| --- | ---: | ---: | ---: |
| Snapshot + restore + journal updates/finalization + durable commit | 336.992250 ms/transition | 6.970833 ms/transition | -97.93% |
| Gross requested allocation bytes | 473,442,466 bytes/transition | 1,372,977 bytes/transition | -99.71% |
| Allocation requests | 3,887,027 requests/transition | 4,488 requests/transition | -99.88% |

[Five alternating sequential timing pairs, three separate allocation pairs, raw samples, immutable binary and source/driver/contract hashes](results/sparse-world-performance-20261113T130034338467Z/summary.json). The timer excludes database/cache setup, parent generation/commit, expected-state construction, final full export/checks and cleanup. Account-load counts are captured before diagnostic export; sparse transitions load at most three accounts while materialized mode loads all 10,000. This is synthetic storage/journal work, not a signed-transaction or native-block execution benchmark. The [contract](configs/benchmark-sparse-world-v1.json) was frozen before measurement.

### Unchanged sixteen-block native branch

| Metric | Previous pinned-state source | Sparse account/storage source | Change |
| --- | ---: | ---: | ---: |
| Elapsed time (ms/block) | 11.226416 | 11.230744 | +0.04% |
| Gross requested allocation bytes/block | 1,058,914.000000 | 1,053,605.000000 | -0.50% |
| Allocation requests/block | 11,544.000000 | 11,434.000000 | -0.95% |

[7 alternating sequential timing pairs, three separate allocation pairs, raw samples and both frozen sources/binaries](results/indexed-history-performance-20261113T130053956220Z/summary.json). The identical `bench-state-storage.rs` runs against isolated Cargo targets. These source comparisons include the private execution layout change and lazy providers.

### Unchanged 25-event / 24-block private canonical reopen

| Metric | Previous pinned-state source | Sparse account/storage source | Change |
| --- | ---: | ---: | ---: |
| Elapsed time (ms/reopen) | 22.739875 | 22.649459 | -0.40% |
| Gross requested allocation bytes/reopen | 3,337,952.000000 | 3,337,300.000000 | -0.02% |
| Allocation requests/reopen | 10,502.000000 | 10,391.000000 | -1.06% |

[5 alternating sequential timing pairs, three separate allocation pairs, raw samples and both frozen sources/binaries](results/canonical-reopen-performance-20261113T130200154448Z/summary.json). The identical `bench-canonical-reopen.rs` runs against isolated Cargo targets. These source comparisons include the private execution layout change and lazy providers.

The short native branch includes execution with warm proof work, fresh parent restoration, branch staging and durable execution-head selection. Parsing, genesis/database/cache setup, proof warming and final checks are excluded; canonical catalog publication/network acquisition are outside that timer. Canonical reopen includes fresh database/cache initialization and indexed receipt/frontier recovery after the fixed history, and excludes history construction, import/proof work and final checks. Reopen trusts local native admission receipts; it does not replay/revalidate every historical block.

Every measured change is retained, including regressions. Timing runs sequentially on a shared host and retains load averages. Allocation measurements run separately, count gross requested bytes including reallocations, exclude frees and do not measure retained heap or RSS. Configured execution database cache capacity remains 64 MiB. Do not multiply these ratios by earlier kernel/recovery ratios. No Go speed, whole-client TPS, cold-disk or public-network claim follows.

The qualified source passes 241 default and reduced Go-denied release tests over 192 Rust sources / 22 assets, with zero Go/protoc attempts. Frozen persisted transitions retain 18,312 Go comparisons over 1,371 cases / 2,706 roundtrips. Native canonical qualification remains 488 checks / 24 distinct early-profile blocks; separate Rust processes retain 165 assertions. Additional real old/new Rust executables preserve every byte hash in the old datadir when the new format refuses it and confirm the old binary still reopens. The 65,537-account and 4,097-slot storage tests are synthetic local versions, not additional network blocks. [Implementation, errors, private disk boundary and remaining scope](NATIVE_LAZY_WORLD_RESULTS.md).

## 2026-11-13: native ETX, economic metadata and raw-lockup reads on demand

The private canonical writer now restores ETX queue, economic metadata and raw-lockup providers from pinned snapshots, alongside the existing account/storage/Qi readers. Persistence visits touched records. Queue and lockup read failures remain unavailable through incoming dispatch, nested VM rollback and authority metadata updates. The format stays canonical 5 / keyed marker 3 / branch metadata 5 / execution codec 2. Full consensus/public sync, complete EVM/resource behavior and production scale/pruning remain unfinished.

### Same-source synthetic auxiliary-state configurations

One current Rust binary uses explicit materialized and sparse modes against the same codec-2 database. A fixed 5,000-ETX / 5,000-lockup parent receives two queue pops, one queue push, one economic metadata update and three lockup mutations, followed by a durable child commit. Exact final queue order/indexes, metadata, lockups, consensus roots, local digest and version ID must agree.

| Metric | Materialized auxiliary state | Sparse auxiliary state | Change |
| --- | ---: | ---: | ---: |
| Snapshot + restore + auxiliary updates + durable commit | 61.897375 ms/transition | 7.726083 ms/transition | -87.52% |
| Gross requested allocation bytes | 65,197,619 bytes/transition | 1,602,478 bytes/transition | -97.54% |
| Allocation requests | 741,223 requests/transition | 5,581 requests/transition | -99.25% |

[Five alternating sequential timing pairs, three separate allocation pairs, raw samples, immutable binary and source/driver/contract hashes](results/sparse-aux-performance-20261113T140150189599Z/summary.json). The timer excludes database/cache setup, parent generation/commit, expected-state construction, final full export/checks and cleanup. Record-load counts are captured before diagnostic export; sparse transitions decode at most two present queue records and two present lockups while materialized mode decodes all 5,000 of each. This is synthetic auxiliary-state work, not a signed-transaction or native-block execution benchmark. The [contract](configs/benchmark-sparse-aux-v1.json) was frozen before measurement.

### Unchanged sixteen-block native branch

| Metric | Previous lazy-account/storage source | Sparse ETX/lockup source | Change |
| --- | ---: | ---: | ---: |
| Elapsed time (ms/block) | 11.339062 | 11.595994 | +2.27% |
| Gross requested allocation bytes/block | 1,053,605.000000 | 1,058,726.000000 | +0.49% |
| Allocation requests/block | 11,434.000000 | 11,448.000000 | +0.12% |

[7 alternating sequential timing pairs, three separate allocation pairs, raw samples and both frozen sources/binaries](results/indexed-history-performance-20261113T140206115801Z/summary.json). The identical `bench-state-storage.rs` runs against isolated Cargo targets. These comparisons retain the same disk layout and common harness, with the auxiliary-state providers added to the current source.

### Unchanged 25-event / 24-block private canonical reopen

| Metric | Previous lazy-account/storage source | Sparse ETX/lockup source | Change |
| --- | ---: | ---: | ---: |
| Elapsed time (ms/reopen) | 21.767708 | 21.525125 | -1.11% |
| Gross requested allocation bytes/reopen | 3,337,281.000000 | 3,357,895.000000 | +0.62% |
| Allocation requests/reopen | 10,391.000000 | 10,451.000000 | +0.58% |

[5 alternating sequential timing pairs, three separate allocation pairs, raw samples and both frozen sources/binaries](results/canonical-reopen-performance-20261113T140323344811Z/summary.json). The identical `bench-canonical-reopen.rs` runs against isolated Cargo targets. These comparisons retain the same disk layout and common harness, with the auxiliary-state providers added to the current source.

The short native branch includes execution with warm proof work, fresh parent restoration, branch staging and durable execution-head selection. Parsing, genesis/database/cache setup, proof warming and final checks are excluded; canonical catalog publication/network acquisition are outside that timer. Canonical reopen includes fresh database/cache initialization and indexed receipt/frontier recovery after the fixed history, and excludes history construction, import/proof work and final checks. Reopen trusts local native admission receipts; it does not replay/revalidate every historical block.

Every measured change is retained, including regressions. Timing runs sequentially on a shared host and retains load averages. Allocation measurements run separately, count gross requested bytes including reallocations, exclude frees and do not measure retained heap or RSS. Configured execution database cache capacity remains 64 MiB. Do not multiply these ratios by earlier kernel/recovery ratios. No Go speed, whole-client TPS, cold-disk or public-network claim follows.

The qualified source passes 247 default and reduced Go-denied release tests over 196 Rust sources / 22 assets, with zero Go/protoc attempts. Frozen persisted transitions retain 18,312 Go comparisons over 1,371 cases / 2,706 roundtrips. Native canonical qualification remains 488 checks / 24 distinct early-profile blocks; separate Rust processes retain 165 assertions. Actual previous/current Rust binaries add 9 unchanged-format reopen/append assertions across five already-frozen blocks. Older format-4 datadirs retain the existing 11 preservation/refusal checks. The 16,385-entry queue test uses synthetic local state; it adds no network blocks. An initial workspace run aborted in the unchanged generic state-db test; the failed log remains retained and is not counted as qualification. [Implementation, typed failures, unchanged private disk boundary and remaining scope](NATIVE_LAZY_AUX_RESULTS.md).

## 2026-11-13: activated local consensus composition and a bound execution handoff

The private canonical execution path now consumes a prepared hierarchy result bound to the exact block and reviewed chain/location/genesis, taking inbounds directly from that result. This removes one inbound-list clone from the handoff. No new timing or allocation improvement is claimed for this source. The first qualified draft retained the 3339890-byte official allocation table in the private runner. Exact allocation-policy binding and compile-time policy specialization reduce its file size from 11284720 to 7916240 bytes (-29.85%), versus 7915920 bytes in the preceding installed source. [Immutable binaries, manifests and exact file-size evidence](results/activation-layout-20261113T151719369084Z/summary.json). This is a file-size result, with no runtime/RSS implication. Earlier numbers retain their original source hashes, artifacts and workloads; they are not current-source performance qualification.

New conditional coverage adds four sealed mixed Qi/Quai/standard-ETX blocks at heights 259,201-259,204 and 33 valid-work mutations checked against pinned official Go. Both memory and lazy persisted modes pass 772 checks, with 66 rejected attempts, nine state roundtrips and eight writer reopens. The synthetic height-259200 checkpoint and supplied historical slices are unadmitted inputs. The existing private canonical corpus stays at 24 distinct genesis-linked early blocks; full consensus/public sync and production efficiency remain unqualified. [Implementation and exact limits](NATIVE_ACTIVATION_RESULTS.md).

## 2026-11-15: activated coincident hierarchy with lazy local recovery

Native zone, region and prime preparation now composes with exact-bound activated Qi/Quai/ETX execution. Two branches with 24 distinct conditional blocks pass 4068 checks, including 492 rejected attempts across memory/lazy modes, 66 state roundtrips and sixteen writer reopens. Hierarchy outputs and execution state persist together; pinned reads exclude competing-fork history. The supplied height-259200 checkpoint grants no canonical authority, and the early canonical runtime is unchanged. [Scope and source-bound results](NATIVE_COINCIDENT_RESULTS.md).

No new latency, throughput, allocation or RSS improvement was measured. Historical performance numbers retain their original source/workload qualification. Current private-node file size is 7916240 bytes (0 bytes versus the installed baseline). [Immutable artifact comparison](results/coincident-work-20261116T001208621057Z/binary-layout.json).

## 2026-11-15: genesis-only runtime and bounded forward acquisition

The experimental v2 profile imports 8192 native blocks from the declared empty genesis, with 122930 replay/reopen checks and nine writer reopens. Independent Rust processes acquire the same history in 64 windows, beyond the older 4095-unknown-block operation bound. Forward windows cap raw bundles at 128 and response payload at 32 MiB; native admission remains per block. Discovery releases its pinned catalog view before import. The default v1 profile and frozen RPC are unchanged. [Source-bound results and limits](NATIVE_GENESIS_SYNC_RESULTS.md).

On the shared 96-block subworkload, the uninstalled forward prototype used 248 application requests; the current implementation uses 152 (38.7097% fewer) by requesting numbered blocks directly. Both immutable binaries use the same correctness driver and frozen Go corpus. The baseline is not the preceding installed backward-sync path. These are logical request counts, not latency, throughput, allocation or RSS results. No new runtime timing or heap gain is claimed. Existing speed/allocation figures keep their original source hashes and workloads.

## 2026-11-16: local full-header proof and epoch-cache reuse

The fixed 16-block first canonical branch produces identical heads and state roots in both modes. Native admission has 24 full-header proof-cache misses with raw inputs and 8 with sealed local proof hints; 16 exact proofs are reused. The 66.7% reduction applies to admission-stage cache misses. It is not an end-to-end speedup, kernel-operation count, heap or RSS result. The epoch cache is shared as one immutable local allocation and retained across acquisition windows. No new timing or allocation gain is claimed while the offline Go oracle shares the host. [Correctness, raw counter evidence and scope](NATIVE_PROOF_REUSE_RESULTS.md).

## 2026-11-16: full proof reuse for inclusion and rewards

The fixed 8192-block source and independent receiver each report 8189 exact full proofs reused for weaker inclusion/reward work. Each reuse replaces another native work computation; changed headers and weaker-only results cannot enter that path. This is a logical operation count, not an end-to-end speedup, heap or RSS measurement. The actual live 1024-block streaming pilot also passes 54 temporary EOF waits and final gzip/source/raw hashes, then a separate receiver reuses its locally executed datadir across a restart (62 assertions). No runtime timing gain is claimed while the offline Go oracle and full native follower share the host. [Source-bound evidence and scope](NATIVE_WORK_REUSE_RESULTS.md).

## 2026-11-16: composed ETX-family coverage

All seven defined incoming kinds now pass 4068 conditional comparisons with signed Quai/Qi, persistence and hierarchy checks. This expands correctness coverage; it does not measure a timing, heap or RSS improvement. Existing proof-reuse counts are unchanged. [Evidence and limits](NATIVE_ETX_FAMILY_RESULTS.md).

## 2026-11-16: native prime controller arithmetic

Five prime controller families pass 3200 Go comparisons; miner-difficulty validation shares the bounded 512-bit implementation. This expands correctness coverage; it does not measure a timing, heap or RSS improvement. Existing proof-reuse counts are unchanged. [Evidence and limits](NATIVE_CONTROLLER_RESULTS.md).

## 2026-11-16: branch-bound native token-choice window

The 4000-entry native window passes 4216 Go updates. Each update copies at most 64 logical rows and writes one changed row plus metadata; rate feedback reuses the incrementally maintained sum. These bounds exclude handles, trie nodes and database bookkeeping. This expands correctness coverage; it does not measure a timing, heap or RSS improvement. Existing proof-reuse counts are unchanged. [Evidence and limits](NATIVE_CHOICE_WINDOW_RESULTS.md).

## 2026-11-16: shared decoded-history cache measurement

Two sequential fresh 8192-block runs per source used order F806, G72, G72, F806. Median wall time was 625.616 -> 606.697 s (3.024% lower); native child CPU was 435.218 -> 414.840 s (4.682% lower). Baseline runs were 670.840 / 580.392 s; candidate runs were 607.300 / 606.094 s. Ranges overlap and shared-host load changes, so a robust speedup is **not established**. Every run retained all 106523 checker assertions, eight writer reopens, exact input hashes and Go/protoc denial. No heap, allocation or RSS claim is made. The measurements belong to those two exact source builds, not the current parent-controller build. [Raw benchmark](results/history-memo-benchmark-20261116T092322175878Z/summary.json).

## 2026-11-16: storage stability and linked prime-forward qualification

The current installed source keeps thin LTO and one codegen unit. It changes 16-way MPT branch initialization to incremental construction to avoid an optimization-sensitive macOS 26.0-26.3 arm64 allocator failure, and explicitly unlocks the private writer lease before an inherited duplicate descriptor closes. Eight fresh release processes pass **2,048 repetitions / 3,096,576 path-copy updates / 67,584 materialization-reloads**. The standalone and Go-denied builds pass **281 tests**, including the deterministic inherited-descriptor regression. These are stability/correctness results, not speed or memory improvements.

Diagnostic profile timings observed while isolating the allocator failure were 5.60 s (LTO off, one codegen unit), 5.58 s (thin LTO, 16 codegen units), 5.96 s (fat LTO, one codegen unit), and 6.80/6.89 s for two fixed thin-LTO one-unit stress runs. The workloads were diagnostic runs on a shared host, not a preregistered benchmark, so no performance ranking or percentage gain is claimed. ASan masked the host allocator failure and is not counted as qualification. No tracked allocation, retained heap or RSS result was collected.

Linked prime-forward qualification covers **256 histories / 1,024 forward steps / 23,040 ETX encodings / 10,240 reference refunds**, plus 32 persisted updates, 16 writer reopens and eight atomic aborts. This expands correctness coverage only. The full 30-gate qualification also repeats the 8,192-block genesis replay and independent receiver, with 8,192 header proofs reused. [Prime-forward results](NATIVE_PRIME_FORWARD_RESULTS.md) and [storage-stability results](NATIVE_STORAGE_STABILITY_RESULTS.md) retain hashes and scope.

## Next measurements

Measure first-seen versus reused work separately, then complete private block replay and checkpoint I/O on fixed corpora. Incremental persistent state, mixed Quai/Qi/ETX blocks, reorg/restart and peer/RPC throughput need their own correctness gates and comparable baselines. Keep latency, allocations, retained memory/RSS, disk and end-to-end network measurements separate. Preserve regressions and rejected experiments alongside gains.
