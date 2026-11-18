# Optimized trie and writer-lock stability

Two release-only reliability failures were closed without weakening the production optimization profile.

On macOS 26.3 arm64, repeated construction/destruction of a niche-optimized 16-element MPT branch could trigger an intermittent allocator invalid-free under thin LTO with one codegen unit. Branch children are now constructed incrementally. The fixed gate runs eight fresh release processes for **2,048 repetitions, 3,096,576 path-copy updates and 67,584 materialization/reloads**. Every process passed under Rust 1.95.0 on the affected host profile. This preserves trie roots and lookup behavior; it does not claim a speed or memory improvement.

The private journal now explicitly unlocks its writer lease in `Drop`. Closing only the owning descriptor was insufficient when a concurrent fork briefly inherited the same open-file description. A deterministic regression clones the descriptor, drops the journal, reopens the writer while the duplicate remains live, then closes both cleanly. The complete 35-test store suite, process-exit recovery, branch/reorg recovery and the full 281-test workspace pass.

The final qualification also passes 13 native-transport tests, the 8,192-block genesis replay and independent receiver, 30 sequential gates, and nine old-current-old storage compatibility assertions. The source stays Go-free in default build/distribution/runtime. Global consensus and public sync remain open.

Evidence:

- `results/mpt-stability-20261116T184736813710Z/summary.json`
- `results/prime-forward-qualification-20261116T184306760849Z/summary.json`
- source build `7f42f8560bfc3c71be5d7f07f135eab4b19e9ca0541109713a13e37c5737940b`

Diagnostic timings in the performance ledger are retained as non-benchmark observations. No heap/RSS result was collected.
