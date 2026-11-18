Current extension: [sparse account and contract storage](NATIVE_LAZY_WORLD_RESULTS.md). Counts and timings below identify the preceding pinned-state source.

# Pinned execution-state database lifetime

Full consensus and public canonical sync remain unfinished. The reference stays official go-quai v0.57.0, revision `33db82f2891b267ba898ca38bd2dd8bf75cbb072`; Go remains absent from the default build, distribution and runtime. Account/storage/code, ETX and lockup state still materializes. Qi membership, native receipt catalogs and branch ancestry already read on demand.

## Correctness change

An immutable execution-state snapshot now owns its database handle. Previously, closing the writer invalidated a later snapshot read with `Database has been closed`; the failing test source, executable, digest and output are retained under `results/state-lease-before-regression/`. Sparse Qi membership inherits the same lifetime because its provider owns the snapshot. Uncached point reads continue after writer close and after a later child is committed; an old parent still observes its own version.

On Unix, a weak registry indexed by device and inode lets a new writer reuse a database kept alive by snapshots. A separate writer lease refuses a second live writer, including hard-link aliases and concurrent reopen attempts. Snapshots never retain that lease. A wrong-profile open still fails. Other processes continue to use redb's file lock. The generic snapshot ownership change is portable; same-process reopen with pinned readers, inode alias handling and path identity are qualified here on macOS/Unix, not Windows or Linux fault tests.

Writer operations check the file identity. Commit checks both before execution and after its callback, before publication. Replacing the pathname during that callback aborts the transaction; the renamed original retains its old selected head and no new block index. Pinned readers may continue reading the original inode. These identity checks do not claim to eliminate every hostile filesystem race.

The on-disk encoding is unchanged: canonical format 4, catalog schema 1, keyed marker 2, branch metadata 4, hierarchy delta 1 and execution codec 1. No state migration or network protocol change is introduced. Global `Verifier.validate_block` remains Unsupported and public JSON-RPC remains frozen, read-only and unverified.

## Regression and compatibility evidence

Five new tests cover snapshot ownership after writer drop, parent reads across writer reopen/child commits, exactly one winner among six concurrent opens through inode aliases, path replacement during a transaction, and real sparse-Qi provider reads after close/reopen. They preserve stale-head CAS rejection, profile binding and fresh-read corruption checks. These storage tests do not add native network blocks or expand the private consensus profile.

The default and reduced Go-denied builds each pass **232 release tests / 188 Rust sources / 22 assets**. Frozen persisted execution retains **18,312 Go comparisons / 1,371 cases / 2,706 roundtrips**; native canonical qualification retains **488 checks / 24 distinct early-profile blocks**. Separate Rust processes retain **165 assertions**, and frozen RPC/SDK checks remain **70/16**. Source-bound reports are recorded in `results/latest-state-lease-work.json`. Measurements use the unchanged sixteen-block hierarchy/coinbase transition harness and the unchanged 25-event/24-block canonical reopen harness, with the immediately preceding sparse-Qi source as baseline. Both sources use isolated Cargo targets and immutable executed binaries; timer/workload/result checks stay frozen. The performance ledger records all results, including regressions. Gross requested allocations are measured separately from timing and are neither retained heap nor RSS.

Database and cache ownership lasts until the final snapshot drops. This can retain database resources across writer restarts; bounded reader lifetimes and safe pruning remain required work. The execution database cache stays configured at 64 MiB. Configured cache capacity is not a measured resident-memory limit. A process restart naturally releases its own snapshots; no cross-process snapshot transfer is implied.

## Next account and storage provider gate

1. Add typed unavailable-state errors to World/account/slot access. Propagate them through journal undo, nested CALL/CREATE, precompiles and signed settlement; unavailable data must abort the candidate without becoming absence, zero storage, an EVM revert or a successful receipt.
2. Restore a World from a pinned account root and bounded touched-key overlay. Remove Journal's all-account initial-size/finalization scans. Preserve Quai's independent five-field account encoding and storage-size accounting, including nested revert and recreate behavior.
3. Read each account and storage path on demand, verifying account metadata, code hash and slot preimages. Commit only changed account/slot records. Account deletion must drop its storage root without enumerating every slot. Review the preimage index layout and explicitly version any disk-format change before use; refuse unsupported old private formats without silent migration.
4. Run the frozen signed Go differential corpus from sparse World/Qi providers; inject missing/corrupt account, code and storage nodes after admission and across writer reopen. Prove typed failure and no state/gas/head publication. Explicit bounded exports may enumerate only for diagnostics and comparison.
5. Qualify mixed signed native canonical blocks, later fork/economic cases, nonlocal/expansion histories and global coordinator workers. Extend independent private sync beyond the fixed early corpus, then public shadow following and verified live state RPC. Keep partial coverage and the tracked full goal explicit until these gates pass.

## Reproduction

    cargo test --release --locked --offline -p quai-store state_db::tests
    cargo test --release --locked --offline -p quai-execution sparse_qi_pinned_membership
    python3 scripts/build.py
    python3 scripts/verify_rust_build.py
    python3 scripts/verify_persistent_execution.py
    python3 scripts/verify_canonical.py --peer
    python3 scripts/verify_private_processes.py

No production peer, wallet or Go checkout is required for these checks.

The [performance ledger](../../PERFORMANCE_LEDGER.md) records this source comparison with exact medians, raw sample links and scope limits.
