Current extension: [native weak-share and PoEM results](NATIVE_WEAKSHARES_RESULTS.md). Counts and timings below describe an earlier source increment.

# Native full-target uncle qualification

Full consensus and canonical synchronization remain **unfinished**. Native libraries now integrate pre-KAWPOW full-target ProgPoW uncles with early three-context append, parent entropy, rewards, confirmation and incoming coinbases. RPC remains a frozen, read-only unverified view. Official go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the reference; the archived local fee overlay stays inactive.

## Implemented and qualified

The existing [private hierarchy profile](configs/private-hierarchy-v1.json) retains its genesis, namespace and protocol rules. This extends its implemented domain. Full native uncle proofs and inclusion policy run before staging, including ancestor/duplicate/dangling/number checks and body commitments. `strong_uncles::native_components` computes each share's integer excess over the containing block's target entropy, discounted by ancestry distance. It also sums full-proof uncle entropy for the execution commitment. Parent zone PoEM total and non-prime delta now include authenticated pre-fork weight; order and rank do not include that current share weight. These rules follow actual Go `WorkShareLogEntropy`, `UncledLogEntropy` and header/state validation.

The supported path is early, single-zone, expansion zero, with full-target ProgPoW uncles and incoming coinbases. It still excludes weak historical shares, later auxiliary/fork domains, mixed transactions and nonlocal/sibling rollups. General candidate fork-choice scoring with nonempty pre-fork shares remains a separate unqualified boundary. No global admitted-block type or canonical publication/sync authority is granted.

The new explicit Go oracle uses real ProgPoW, computed order, `AppendHeader`, `ValidateBody`, `StateProcessor.Process`, `ValidateState`, PCRC and native ETX collection/filtering. Offline nonce searches create real full-target uncle headers at inclusion distances one and two. Two sixteen-block branches share eight blocks: **24 distinct blocks, 38 active-context bundles and 13 uncle inclusions** across distinct executions. Each complete branch includes eight uncles and consumes twelve derived incoming coinbases; the distinct executions consume **24**. Entropy values, header/manifests/termini, work, rewards, state/UTXO/ETX commitments, typed receipts, branch switching and reopen all match.

[The checker](results/uncles-20261112T200541701060Z/summary.json) records **505 comparisons**. Eight new Go-invalid, valid-candidate-work variants cover duplicate, ancestor, sibling, previously included and wrong-number uncles, bad mix, uncle-root mismatch and incorrect uncled entropy. Go header/body/state methods reject them at the documented stage. Together with the earlier missing/substituted-view and forged-rollup controls, **11 inputs** are rejected without staging or HEAD changes. The same checker retains 29 ordered ETX filter cases, profile/bypass controls and post-cache corruption detection.

A separate candidate with a weak pre-fork share passes actual Go header/body/state validation and has valid full candidate work. Rust returns an explicit weak-share entropy capability error without staging it. Native inclusion work is not promoted into a full-target proof. The missing weak-share path uses reference floating-point exponential discounting and remains unqualified; the valid Go block is not labeled protocol-invalid by this report.

The default and Go-denied builds each pass **185 tests**, covering **157 Rust files and 22 source assets**. The reduced build has no Go checkout and records zero Go/protoc attempts. Frozen RPC/SDK checks retain **70/16** comparisons. [Combined source-bound evidence](results/uncles-work-20261112T201412245555Z/summary.json) records **42,019 component comparisons**; workloads overlap, so this is not a unique-coverage or completion percentage. The default build, distribution and runtime remain independent of Go.

## Parent restore measurement

The hierarchy entrypoint previously restored the complete parent once for hierarchy work and again inside execution. It now passes the same freshly verified `Restored` value through a private internal call, bound to its exact checkpoint. The current call still checks ancestry checksums, state-image/receipt bindings and all current policies. Staging rechecks the immediate parent, and later calls read fresh bytes. This introduces no execution-state or integrity cache across calls.

Five alternating sequential timing pairs and three separate allocation pairs compare the same new uncle implementation before and after this restore change. Corpus, profile, harness and lockfile match; the passing pre-optimization source is frozen. Each process executes sixteen distinct blocks, 25 context appends, eight full-target uncle inclusions and twelve derived incoming coinbases. Both versions produce identical final block/state commitments.

| Metric per block | Duplicate restore baseline | Single restore | Change |
| --- | ---: | ---: | ---: |
| Warm-proof execution and durable commit | 24.549523 ms | 23.625825 ms | 1.04x |
| Gross allocated bytes | 2,075,261 | 1,655,669 | 20.2% fewer |
| Allocation requests | 17,864 | 14,104 | 21.0% fewer |

Full block/uncle and reward-inclusion proofs are warmed before timing. Fixture decoding and genesis setup are excluded. Timed work includes current policy/rollup checks, full execution/images, fresh ancestry checksums, staging and selected-head fsync/rename on a warm filesystem cache. The median timing difference is about 3.8%, with overlapping sample ranges; treat it as modest shared-host evidence. This is a shared-host Rust/Rust private workload; it does not measure cold proof/disk behavior, Go-versus-Rust performance, complete consensus, node TPS, RPC or network sync. Gross Rust requests include full requested reallocations and are sampled separately from timing; retained/live heap and peak RSS are unmeasured. Full images, bounded state and ancestor scans still need production incremental storage/indexing.

[Raw samples, workload and source/binary hashes](results/uncles-performance-20261112T200634720729Z/summary.json) and [complete pre-optimization source](results/uncles-performance-20261112T200634720729Z/baseline-source.tar.gz) are preserved. [The performance ledger](PERFORMANCE_LEDGER.md) keeps prior gains and regressions under their original workloads; figures from different corpora are not cumulative speedups.

## Next and reproduction

Qualify weak-share floating-point entropy and candidate scoring, then broaden nonlocal rollups, mixed Quai/Qi/ETXs, all workshare/fork/expansion histories and economic controllers. Full admission must precede PoEM canonical publication, production incremental state/indexes and competing-node sync/reorg/restart qualification. `Verifier.validate_block` remains Unsupported and component/candidate results remain `fully_validated: false`.

    python3 scripts/build.py
    python3 scripts/verify_uncles.py
    cargo test --locked --offline -p quai-consensus --test uncles
    python3 scripts/verify_rust_build.py

Explicit offline Go regeneration: `python3 scripts/gen_uncles_vectors.py --go-quai ../go-quai --check`. Normal builds/tests never invoke Go. No public peer or wallet action was performed. Rollback and readable content/metadata inspection are recorded in `results/uncles-installation.json`.
