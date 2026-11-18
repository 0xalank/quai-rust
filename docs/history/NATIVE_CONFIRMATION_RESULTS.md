Current extension: [native full-target uncle results](NATIVE_UNCLES_RESULTS.md). Counts and timings below describe an earlier source increment.

# Native hierarchy confirmation and execution

Full consensus and canonical synchronization remain **unfinished**. This increment composes native prime/region/zone append components, authenticated ETX rollups, derived confirmation and incoming coinbase execution before private durable staging. RPC still serves a frozen, read-only unverified view. Official go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the reference; the local fee overlay stays archived and inactive.

## Qualified private boundary

The separately pinned [hierarchy profile](../../configs/private-hierarchy-v1.json) uses difficulty two, chain ID 1337, location 0/0 and expansion zero. Its genesis equals the earlier local-admission profile, but its full profile fingerprint and journal namespace differ. Existing execution-only and local-only profiles remain separate. Weaker entrypoints cannot stage into a hierarchy store, and reopening it under the local-only profile fails.

`confirmation::Hierarchy` composes native computed order, full header work, parent entropy/difficulty and header/body policy, parent manifests/interlinks and PCRC in every active context. Coincident witnesses must contain the same full work header and body header as the zone candidate. Manifests select previously stored branch history, rather than a supplied ETX list. The zone-parent manifest produces ordered outbounds; cross-prime filtering derives the rollup commitment. Prime/region manifests and verified ancestry determine newly confirmed ETXs in reference order, preserving duplicates. The execution list comes from the **parent zone block's** confirmed list, including the reference delay between confirmation and consumption.

The integrated runtime remains early, single-zone and pre-transaction, with no uncles/workshares and only incoming coinbase ETXs (type 1). Nonlocal/sibling history, expansion, mixed Quai/Qi/other incoming ETX kinds, later economic controllers/forks and general consensus admission need further qualification. Standalone filtering differentials cover more ETX types and destinations; they do not establish those execution domains.

`PrivateBranch.open_hierarchy` and `execute_hierarchy_candidate` execute and compare commitments before durable staging. Version-two payloads retain higher-context witness bytes under the same checkpoint checksum as the complete state image, typed receipts and execution history. Local-only blocks need no higher-context bytes and keep the existing encoding. Reopen or branch changes rebuild hierarchy components from the selected ancestry's witnesses. Recovery trusts the local content-addressed journal; it is not peer snapshot admission. Private head selection remains explicit and grants no PoEM or RPC canonical authority.

## Differential and integrity evidence

The explicit Go adapter uses real native ProgPoW, computed order, `AppendHeader`, `ValidateBody`, `StateProcessor.Process`, `ValidateState`, PCRC and native ETX collection/filter methods. Offline fixture nonce searches select the required orders; no fake proof or injected order cache is used. Go also checks the pending cross-prime rollup against its commitment.

Two sixteen-block branches share eight blocks: **24 unique blocks, 32 oracle rows and 38 active-context bundles**. Native work hashes, manifests/termini, returned dominant PCRC values, confirmed/incoming ETX order, state/UTXO/ETX roots and typed receipts match Go. Each branch confirms and consumes eight incoming coinbases; the distinct executions total **16**. Reopen/rebuild and an eight-block undo/eight-block forward private branch switch pass.

[The checker](../../results/confirmation-20261112T191407950486Z/summary.json) records **430 comparisons**. It rejects missing higher-context views, a substituted other-branch view and one forged rollup with valid native work without staging or changing HEAD. The forged rollup passes Go header/body checks but fails the actual pending-rollup commitment check. The same checker includes **29 Go filter cases** cover all seven ETX types, three destinations and an ordered duplicate, plus cross-prime rules. These are bounded component cases, not a whole-protocol coverage percentage.

A single hierarchy prefix is cached by the exact content-addressed parent checkpoint. Every attempt first restores and checks the journal ancestry; all current candidate/header/clock/rollup and execution checks still run. The cache is installed only after successful durable staging; branch misses and reopen rebuild it, and preparation/execution errors discard it. A test populates the prefix, corrupts its persisted checkpoint and requires rejection with unchanged HEAD. Existing stale/corrupt journal, process-exit recovery and profile-boundary tests also pass.

The default and reduced Go-denied builds each pass **184 tests**, across **153 Rust source files and 22 source assets**. The reduced build has no Go checkout and records zero Go/protoc attempts. Frozen RPC and both pinned SDKs retain **70/16** comparisons. [Combined evidence](../../results/confirmation-work-20261112T192223713248Z/summary.json) binds the current build and all component reports: **41,514 component comparisons**, with overlapping workloads and no completion percentage. Go remains an explicit offline oracle, absent from the default build, distribution and runtime.

## Measured prefix reuse

Five alternating paired timing rounds and three separate allocation rounds compare the same new hierarchy code before and after the prefix cache. The baseline is frozen with its passing confirmation test; corpus, profile, lockfile and harness match. Each process executes and durably selects sixteen distinct blocks with 25 native context appends and eight derived incoming coinbases.

| Per-block metric | Previous Rust median | Cached-prefix Rust median | Change |
| --- | ---: | ---: | ---: |
| Warm-work execution and durable commit | 28.183757 ms | 23.939349 ms | 1.18x |
| Gross allocated bytes | 3,772,498 | 1,836,551 | 51.3% fewer |
| Allocation requests | 45,203 | 16,245 | 64.1% fewer |

The timer includes current policy/confirmation checks, complete execution/state images, fresh journal ancestry checksums, staging and HEAD fsync/rename. Full header and reward-inclusion proofs are warmed before timing; fixture decoding and genesis setup are also outside the timer. Filesystem caches are warm. This is a Rust/Rust private workload, not a first-seen-proof, Go-versus-Rust, cold-disk, full-consensus, node-TPS, RPC or network-sync measurement. Raw shared-host samples retain load averages.

Allocation counts measure gross Rust requests, with reallocations charged at the full new size. They are separate from timing and do not measure live/retained heap or RSS. Prefix reuse retains an additional bounded hierarchy graph; it trades retained state for fewer reconstructions. Histories/candidate indexes retain their existing per-container limits and 4,096-record limit; retained ETX bytes have a 32 MiB logical budget, with container/allocator overhead additional. No retained-memory/RSS improvement is claimed. Recovery still scans complete ancestry, and the private writer still produces full state images rather than a production incremental database.

[Raw measurements, source/binary hashes and contracts](../../results/confirmation-performance-20261112T191430267556Z/summary.json) and [complete pre-cache source](../../results/confirmation-performance-20261112T191430267556Z/baseline-source.tar.gz) are preserved. [The performance ledger](../../PERFORMANCE_LEDGER.md) retains earlier gains, regressions and their distinct scopes.

## Next gates and reproduction

Extend this profile to nonlocal/sibling rollups, all integrated incoming types, workshares, expansion and fork histories. Broaden whole-block mixed Quai/Qi/ETX execution, then construct a sealed full admitted-block type. `Verifier.validate_block` remains Unsupported and component/candidate results remain `fully_validated: false`. Connect full admission to native PoEM canonical publication, durable incremental state/indexes and private competing-node sync/reorg/restart qualification before claiming a validating network node.

    python3 scripts/build.py
    python3 scripts/verify_confirmation.py
    cargo test --locked --offline -p quai-consensus --test confirmation
    python3 scripts/verify_rust_build.py

Explicit offline Go regeneration: `python3 scripts/gen_confirmation_vectors.py --go-quai ../go-quai --check`. Normal builds/tests never invoke it. No public peer or wallet action was performed. Installation rollback and readable content/metadata inspection are recorded in `results/confirmation-installation.json`.
