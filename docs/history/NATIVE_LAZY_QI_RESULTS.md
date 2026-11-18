Current extension: [pinned execution-state lifetime](NATIVE_PINNED_STATE_RESULTS.md). Counts and timings below identify the preceding qualified source.

# Qi state on demand

The native private canonical path now restores Qi membership as a pinned database reader with a write overlay. It no longer enumerates the UTXO trie before executing a block, copies the complete UTXO map for a state clone, or visits unrelated UTXO keys when persisting that transition. Quai accounts/storage, ETX and lockup state still materialize bounded maps. Global full consensus and public sync remain unfinished.

## Read and write contract

A UTXO view now returns a fallible owned-or-borrowed value. A trie proof of absence returns no value. A missing database node, malformed record, wrong outpoint prefix, invalid denomination/lock encoding or storage error returns Unavailable. Signed-Qi input checking, settlement, incoming UTXO creation, historical trimming and zone execution preserve that distinction. No provider failure becomes an empty UTXO or zero balance. Duplicate inputs are still rejected before any lookup.

Restoration verifies the stored local root-set digest, state metadata, independent MuHash/count commitment and the raw membership trie root. It does not count every leaf on reopen: the local native version already binds the raw count maintained by its completed transitions. These are trusted local execution versions, not peer-imported snapshots. Each accessed path and value is verified against the pinned root. Unrelated local corruption is discovered on access, rather than by a full startup scrub.

The immutable base provides outpoints on demand and caches at most 4,096 present/absent lookup results. Record encoding bounds limit each value; cache bookkeeping, vectors and allocator overhead are additional memory. A state clone shares the base and overlay. Writes detach only the overlay, which has a one-million-key operation/view bound. Native persistence compares the touched keys and writes their changes in the same durable transaction as the named roots and metadata. It does not enumerate the base membership set.

Raw membership count and the consensus MuHash/count remain independent. Legacy duplicate overwrite and auxiliary-hash behavior are preserved, including accumulator/count ghosts. Adds, removals, trims and multi-key deltas preflight fallible reads before publishing mutations. A storage-failure test uses a real signed-Qi fixture and checks that neither state nor gas changes. Materialized legacy states retain their previous capacity bound; a stored base is not limited to that total count. Explicit legacy-image/diagnostic export remains bounded and can refuse a base too large to materialize.

The disk encoding is unchanged: canonical format 4, catalog schema 1, keyed marker 2, branch metadata 4, hierarchy delta 1 and execution codec 1. Compatible existing format-4 local data remains readable. The Rust `UtxoView` read API changes to a fallible result; the frozen public JSON-RPC behavior is unchanged.

## Qualification

New storage tests restore a 5,000-UTXO parent with zero point-record decodes, read four specified points, stage removals/legacy overwrite ghosts and verify the exact recovered membership, accumulator and counts. A pinned parent remains unchanged after the child commit. Separate malformed-value tests reject wrong key/width, denomination, lock length and trailing bytes as unavailable state. These are synthetic storage cases, not additional native blocks.

The existing persisted transition bridge now executes from sparse Qi state before and after each transition. The frozen 1,371 signed Quai/Qi/incoming cases retain all 18,312 Go comparisons, 2,706 roundtrips and 36 rejected incoming transitions. Its exact-membership comparisons explicitly export the small fixture states; that diagnostic export is outside the native execution path. The native canonical corpus retains its 488 checks over 25 events / 24 distinct early private blocks. The final default and reduced Go-denied builds each pass **227 release tests / 188 Rust sources / 22 source assets**, with zero Go/protoc attempts. Separate Rust processes retain **165 assertions**, and frozen RPC/SDK comparisons remain **70/16**. Exact source-bound reports are pointed to by `results/latest-lazy-qi-work.json`.

Full mixed-transaction/later-fork/nonlocal/expansion canonical admission, complete global coordinator workers, lazy account/storage/code and ETX/lockup providers, receipt/log indexes, pruning and public automatic sync remain open. `Verifier.validate_block` stays Unsupported. The default RPC continues to serve an unverified read-only snapshot.

## Measurement contract

The new storage comparison uses one current Rust executable with explicit materialized and sparse Qi modes. Both start from the same synthetic 50,000-UTXO parent, remove four outpoints, insert four and commit an identical durable child version. Exact final raw membership, accumulator/count, local root-set digest and version identity must match. This is storage work; signatures, complete block admission and network sync are qualified separately and are not timed here.

Five alternating sequential timing pairs and three separate allocation pairs run after the correctness matrix. The timer includes fresh snapshot creation, parent restoration, cloning, membership updates and durable persistence. Initial database/cache allocation, parent generation/commit, expected-result construction, final full export/checks and cleanup are excluded. Gross requested allocations include reallocations and are not retained memory or RSS. Source/binary/driver hashes, the frozen source archive, raw samples and host load are retained. No cold-disk, Go-versus-Rust, whole-node TPS or real-network claim follows from this synthetic workload.

The performance ledger will retain this experiment alongside the earlier private canonical reopen improvement and short-workload regression. Source-bound evidence is pointed to by `results/latest-sparse-qi-performance.json` after completion.

## Reproduction

    python3 scripts/build.py
    python3 scripts/verify_rust_build.py
    python3 scripts/verify_persistent_execution.py
    python3 scripts/verify_canonical.py --peer
    python3 scripts/verify_private_processes.py
    python3 scripts/build_private_node.py
    python3 scripts/benchmark_sparse_qi.py

No Go executable, checkout, production peer or wallet is required by these commands. Go remains the explicit offline reference used to freeze compatibility vectors.

The qualified synthetic transition measured **139.130 → 30.205 ms (4.61x)** and **-92.69% gross requested bytes**. The [performance ledger](../../PERFORMANCE_LEDGER.md) preserves the short native workload result and all scope limits.
