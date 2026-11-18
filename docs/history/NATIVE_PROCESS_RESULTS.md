# Independent native private-node processes

The optional private Rust node now synchronizes and selects its canonical state across **independent OS processes**. Persistent checksummed peer identity, bounded local controls, cancellation and clean shutdown are implemented. **Global full consensus and public canonical sync remain unfinished; the tracked goal remains active.** Official go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the source reference, with the local fee overlay inactive. Go is absent from normal build/distribution/runtime and remains an explicit offline oracle.

## Runtime and acquisition

`private-node` is an optional native-transport example, packaged separately as [quai-private-node](../../dist/private-node/README.md). It starts the immutable early private-hierarchy profile from its declared genesis, opens a native canonical store and serves raw locally admitted work objects over enrolled IPv4-loopback Noise/Yamux peers. The runner composes native work/header/body/hierarchy/confirmation/state checks with native frontier selection and atomic canonical publication. Peers never supply trusted entropy, checkpoints or state images. This extends the [native private canonical path](NATIVE_CANONICAL_RESULTS.md); it does not broaden its consensus domain.

A source address and announced head are supplied explicitly through bounded local JSON control. The runner does not discover or follow production heads automatically. Its control channel is separate from JSON-RPC. It allows one import/acquisition worker, a queue of eight control messages, 64 KiB control lines and 64 MiB local bundle packs. Existing stream, connection, three-MiB frame, acquisition byte/block/deadline and native history/catalog limits remain. Catalog and state images remain bounded prototype storage.

Peer keys persist in a versioned, checksummed, owner-only file. A complete pending key write can resume without changing the peer ID. Corrupt, symlinked, permissively readable or ambiguous identity files fail closed. The canonical writer lock is acquired before identity creation/recovery. Data directories retain strict profile checks and are not silently migrated.

Cancellation is checked between atomic block publications. A valid prefix remains committed and can resume; an in-flight block completes atomically. Shutdown cancels acquisition/import, drains stream tasks and releases the writer lock. The local input reader uses a dedicated console thread. The first process test exposed that Tokio stdin could keep runtime shutdown alive after `stop` was acknowledged; the dedicated reader fixes this and the clean-exit/reopen checks now pass. The console thread owns no chain state.

## Qualification

[Source-bound evidence](../../results/private-processes-20261113T050448168865Z/summary.json) records **117 assertions**. At least three Rust nodes run concurrently in different OS processes with distinct data directories, verifier instances, memory and proof caches. The Python supervisor supplies only raw block packs/local controls and compares output with the frozen Go corpus. Its child PATH denies Go, gofmt and protoc; no denied command is attempted. Temporary identities and databases are discarded after testing and no identity key is included in exported artifacts.

The primary acquisition imports **24 distinct native blocks in 38 raw-object requests / 61,603 response payload bytes**, matching the previously qualified inputs. Native frontier selection performs one higher-work reorg, including the height-15 branch overtaking height 16. Head and EVM/ETX/UTXO roots match Go. Those counters cover the two primary acquisitions and exclude the extra restart, crash, cancellation and unavailable-head drills, plus TCP/Noise/Yamux overhead.

Two clean receiver restarts and one source restart preserve peer IDs and exact selected state. A restarted source serves the missing competing branch over a new connection. A separate receiver is killed with SIGKILL after durable admission progress; restart recovers an exact Go-matching complete prefix, resumes raw acquisition and selects the competing branch. Another active import is cancelled, reopens at its exact completed prefix and resumes successfully. Tests also reject a second writer, corrupt/readable identity files, oversized control input and a public peer address before transport; an unavailable remote head cannot mutate canonical state. Complete pending identity recovery also preserves the peer ID.

Nine native-transport library tests pass, including cancellation before publication, cancellation after one completed native block, request correlation, poisoned oversized streams and manual gossip validation. Default and reduced Go-denied builds each pass **194 release-profile tests** over **169 Rust sources / 22 source assets**. The reduced source has no Go checkout and zero Go/protoc attempts. Frozen RPC/SDK checks remain **70/16**. [Combined evidence](../../results/process-work-20261113T051026074109Z/summary.json) retains **49,144 component comparisons** across overlapping corpora/projections; the process assertions are reported separately. Neither is a coverage or completion percentage.

## Efficiency and packaging

A locally admitted head now needs no dial or request stream. This is checked with an unenrolled peer in the library and with an offline source process in the node test. The result preserves the current canonical generation rather than allowing a known losing head to force selection. The existing single outstanding request, bounded queues and drained stream tasks remain.

The optional [eight-file private-node bundle](../../dist/private-node/README.md) contains one Rust executable, immutable profile, capability metadata and licenses/checksums. It is separate from the default frozen-view RPC bundle. Rebuild/qualification scripts bind Rust source/assets, lockfile, corpus, binary and transcript hashes. No new comparative throughput, CPU, allocation or RSS benchmark was run; no speedup ratio is claimed. The [performance ledger](../../PERFORMANCE_LEDGER.md) preserves earlier measured gains and regressions under their original contracts.

## Remaining full-goal work

The runtime still uses the fixed early single-zone, expansion-zero profile and incoming coinbases. Mixed signed Quai/Qi/all ETX kinds, nonlocal/sibling histories, later auxiliary/economic/fork boundaries and expansion require integrated admission. Complete multi-zone coordinator eligibility, leader search and pending generation workers remain open; the hash-only scoring projection still rejects nonempty pre-fork shares. Production incremental state/indexes/pruning, sustained malformed/slow peer/fault workloads, automatic head following, an explicit public target and live validated RPC remain unqualified. `Verifier.validate_block` still returns Unsupported. Only the Rust-only build readiness gate is passed.

    python3 scripts/build.py
    python3 scripts/verify_rust_build.py
    python3 scripts/verify_private_processes.py
    python3 scripts/build_private_node.py
    ./dist/private-node/quai-private-node --profile private-hierarchy-v1 --datadir /private/tmp/quai-node

Loopback peer tests need permission to bind local sockets. No public connection, production mining or wallet operation occurred. Installation rollback and readable content/metadata inspection are recorded in `results/process-installation.json`.
