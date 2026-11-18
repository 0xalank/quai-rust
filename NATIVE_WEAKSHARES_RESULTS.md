# Native weak-share entropy and scoring

Full consensus and canonical synchronization remain **unfinished**. This increment closes the earlier weak pre-fork share capability gap within the pinned early private profile and adds native candidate entropy/scoring for nonempty pre-fork shares. RPC remains a frozen, read-only unverified view. Official go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the reference; the archived local fee overlay stays inactive.

## Compatibility change

Go's `TwoToTheX` uses a cubic approximation with binary floating-point constants and rounding after each operation. Replacing it with a host exponential or a higher-precision final calculation would change consensus values. `weak_float` reproduces the pinned coefficient bits, precision propagation, nearest-even rounding and final integer truncation using bounded unsigned integers. The runtime performs no host floating-point arithmetic; the three constants retain their exact binary64 representations.

The arithmetic domain is a Q64 entropy deficit from zero through 256 bits and a bounded distance up to four. Effective calculation precision is 65 through 73 bits; coefficients retain binary64 precision and integer constants follow Go's precision rules. Shift/multiplication and output bounds return capability errors. The optimized workspace is 256 bits; the independently run pre-optimization source uses 512 bits with the same algorithm. Go arithmetic projections supply **5,875 exact deficit/distance cases**, including fractional, integer, precision and near-boundary inputs. Two resource guards bring arithmetic checks to **5,877**. These controlled arithmetic cases do not authenticate any work or historical chain.

`prefork_shares` now derives both strong and weak weights from native inclusion work and ancestry. Full-target uncles alone contribute to `UncledEntropy`; weak inclusion work never populates the full-proof memo. Parent zone PoEM total and non-prime delta include the derived share weight. `scoring::native_entropy_components` and native candidate scoring now handle this weight on supplied ancestry. The older hash-only projection still refuses nonempty pre-fork share scores because it has no authenticated share work. All score/component results retain unverified-history/full-validation boundaries. `strong_uncles` remains a compatibility alias for the expanded component API.

The formerly unsupported Go-valid weak-share alternative now stages successfully while preserving selected HEAD and matching Go state roots. Its share header still fails a full-target block-proof check. The older strong-uncle corpus is retained byte-for-byte; its historical `rust_supported: false` field describes the earlier source and is superseded by the current native acceptance report.

## Integrated real-work evidence

The new offline Go oracle uses actual native ProgPoW, computed order, header/body/state validation, PCRC, reward processing and ETX confirmation/filtering. Weak shares occur at distances zero, one and two, including siblings permitted by the reference. Two sixteen-block branches share eight blocks: **24 distinct blocks, 38 context bundles, 10 weak shares and 13 full-target uncle inclusions** across distinct executions. They execute **30 derived incoming coinbases** and qualify **15 local native candidate scores**. Native total/delta/uncled-delta entropy is compared with Go in all three contexts. Roots, typed receipts, manifest/termini, rewards, private branch switching and complete image/witness recovery match.

[The source-bound checker](results/weakshares-20261112T203829632896Z/summary.json) records **760 native branch/component checks plus 5,877 arithmetic checks: 6,637 total**. Eleven invalid inputs retain atomic rejection controls. One Go-valid alternative that formerly hit the weak-share capability boundary is accepted. Full candidate proofs and weaker share proofs remain separate. No global admitted-block token or canonical-head publication is introduced.

Default and reduced Go-denied builds each pass **187 tests**, across **163 Rust sources and 22 source assets**. The reduced build has no Go checkout and zero Go/protoc attempts. Frozen RPC and both pinned SDKs retain **70/16** comparisons. [Combined evidence](results/weakshares-work-20261112T204859886465Z/summary.json) binds **48,656 component comparisons** to the current build. Overlapping corpora and controlled projections are not a unique-coverage or completion percentage. Go remains an explicit offline oracle, absent from the default build/distribution/runtime.

## Measured arithmetic optimization

Five alternating sequential timing pairs compare the same Go-qualified corpus and algorithm before/after reducing the workspace from 512 to 256 bits. Each process checks inputs during untimed warmup, then runs 94,000 discounts: 5,875 cases repeated sixteen times. Exact checksums match. Parsing/setup are excluded. The frozen baseline is rebuilt and passes the native branch and arithmetic tests before timing.

| Fixed arithmetic kernel | 512-bit workspace | 256-bit workspace | Observed speedup |
| --- | ---: | ---: | ---: |
| Median per discount | 20.399 us | 4.963 us | 4.11x |

Three separate allocation pairs observe **zero Rust heap allocation requests in both measured kernels**. This establishes no allocated-byte reduction between them. Retained memory and peak RSS are unmeasured. The workload is pure arithmetic on a shared host, not a Go benchmark, full block replay, proof, storage, RPC, node-TPS or network-sync measurement. No new end-to-end node speedup is claimed; timings from different corpora must not be multiplied.

[Raw samples, host load, source/binary hashes and contracts](results/weakshares-performance-20261112T203851030678Z/summary.json) and [complete pre-optimization source](results/weakshares-performance-20261112T203851030678Z/baseline-source.tar.gz) are retained. The [performance ledger](PERFORMANCE_LEDGER.md) preserves older block/state measurements and regressions under their original scope.

## Remaining gates and reproduction

The integrated runtime remains early, single-zone, expansion zero, with incoming coinbase execution. Later auxiliary/workshare/fork histories, nonlocal/sibling rollups, mixed Quai/Qi/ETXs and broader economic state need qualification. Candidate scores remain component data on supplied ancestry. A sealed full admitted-block type, PoEM canonical publication, production incremental state/indexes and independent competing-node sync/reorg/restart qualification remain open. `Verifier.validate_block` still returns Unsupported.

    python3 scripts/build.py
    python3 scripts/verify_weakshares.py
    cargo test --locked --offline -p quai-consensus --test weak_float --test weakshares
    python3 scripts/verify_rust_build.py

Explicit offline Go regeneration: `python3 scripts/gen_weakshares_vectors.py --go-quai ../go-quai --check`. Normal builds/tests never invoke Go. No public peer or wallet operation was performed. Installation rollback and readable content/metadata inspection are recorded in `results/weakshares-installation.json`.
