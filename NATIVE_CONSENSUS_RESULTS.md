Current extension: [native weak-share and PoEM results](NATIVE_WEAKSHARES_RESULTS.md). Counts and timings below describe an earlier source increment.

Current work: [mixed-order hierarchy and confirmation results](NATIVE_CONFIRMATION_RESULTS.md). The counts and timings below describe the preceding source increment. Full consensus and canonical synchronization remain unfinished.

# Native consensus/execution work results

Full consensus and canonical synchronization are **not finished**. This increment targets official v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072`. The archived local fee overlay stays inactive. Full block admission still returns Unsupported; RPC serves frozen read-only unverified data.

Native atomic zone execution now composes signed Quai/Qi/incoming settlement, fee ordering/feedback, reward-depth payouts, historical effects and execution commitments. Sixteen linked early private blocks match actual Go `StateProcessor.Process`, including full-size native ProgPoW, reward payouts and incoming coinbases. Another 486 cases qualify the exact extracted official fee/reward branch across selected forks/locations using controlled work inputs. Real auxiliary workshares, mixed transaction/fork histories and complete hierarchy admission need broader integration qualification.

[Source-bound combined summary](results/block-work-20261111T172656208702Z/summary.json); [existing execution checks](results/stateful-20261111T171722690151Z/summary.json); [new payout/block checks](results/zone-execution-20261111T171815650845Z/summary.json); [Rust-only reduced build](results/block-work-20261111T172656208702Z/rust-no-go-build.json).

| Frozen component | Cases | Checks/comparisons |
| --- | ---: | ---: |
| stateful | 1,548 | 20,124 |
| signed Quai settlement | 192 | 2,493 |
| Qi settlement | 756 | 9,483 |
| early blocks | 3 | 40 |
| incoming ETXs | 423 | 6,336 |
| historical effects | 60 | 160 |
| authority transition | 21 | 189 |
| fee/reward branch projections | 486 | 1,350 |
| native-work linked blocks with restored state | 16 | 336 |

These total **40,511 component comparisons**, not unique behavior coverage or a completion percentage. The new block checker rejects **112 forged execution variants atomically** and restores a complete local state image after each accepted block. The default suite passes **173 tests**. The reduced source build covers **141 Rust files and 20 source assets**, with Go/protoc denied and no Go checkout. Current frozen RPC/SDK checks pass **70/16** comparisons. Previous private Noise/Yamux evidence remains three early blocks, one rejected counter and three scratch-checkpoint reopens; it does not qualify the new 16-block runner as network sync.

Complete local images preserve accounts/code/storage/size, raw lockups, pending ETX indices and auxiliary metadata, raw UTXOs, canonical MuHash and independent committed count. A previously trusted local payload digest is mandatory; roots alone cannot authenticate legacy raw-map ghosts/count history. Three dedicated tests cover exact round trips, corrupt/forged structure and unfinished-block rejection. The image codec is not a network snapshot admission path or a durable canonical store. Its 256 MiB limit differs from the existing scratch journal's 64 MiB limit.

The integrated block differential exposed and fixed outbound-root encoding: transaction tries commit typed RLP, while queue/receipt embedding uses enveloped RLP. Incoming/Qi helpers now return created/spent metadata directly, avoiding full-map scans and an extra per-Qi snapshot. Those changes have correctness evidence; no separate scan/allocation speedup was measured.

The inclusion-work memo stores only successful results under full normalized header bytes and node scope, with a 512-entry/4 MiB logical limit. Quorum is fixed for each verifier. Tests require mutated headers and changed scopes to miss, reject invalid work without caching it, and prevent a cached weak inclusion proof from bypassing the independent full-block target check.

Five alternating sequential paired timing rounds measured repeated reward distribution on the same verified history: **19.637059 ms → 0.074347 ms**, **264.13x**. This is reuse of already checked work on a warm epoch; fresh-header/full-replay/TPS/sync performance is unmeasured. [Raw samples, source/binary hashes](results/payout-performance-20261111T172411642184Z/summary.json) and [exact uncached source](results/payout-performance-20261111T172411642184Z/baseline-source.tar.gz) are retained.

The [performance ledger](PERFORMANCE_LEDGER.md) also preserves the prior source-bound state results: 2.30x for VM SSTORE, 5.96x for changed-account/root work and 2.12–3.56x for signed transactions. Those historical measurements do not automatically qualify this later source. Cache-hit snapshot/root figures and the initial noisy 13% root regression remain documented separately. No new allocated-byte, retained-memory/RSS, disk or node-throughput savings were measured. Changed roots still rebuild the upper account trie.

The critical path is broader integrated economic/fork execution, verified hierarchy/inbound eligibility, sealed full block admission, durable complete branch state and PoEM-driven canonical publication/reorg/restart/sync. [Remaining work](NATIVE_CONSENSUS_PROGRESS.md) and [the coverage map](PORTING_MAP.md) retain these gates. No public peer, mining or wallet operation was performed. Installation and bounded content/metadata inspection are recorded in `results/block-installation.json`.
