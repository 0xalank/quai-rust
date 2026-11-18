Current extension: [mixed-order hierarchy and confirmation results](NATIVE_CONFIRMATION_RESULTS.md). The counts and timing below describe this earlier local-only increment.

# Native local-zone header and execution components

Full consensus and canonical sync remain **unfinished**. This increment connects native header/placement components to private execution and durable staging for a separately pinned local-zone profile. It does not promote the default frozen read-only RPC view to validated state. Official go-quai v0.57.0 `33db82f2891b267ba898ca38bd2dd8bf75cbb072` remains the reference; the local fee overlay stays inactive.

## Implemented boundary

`configs/private-admission-v1.json` has a distinct genesis and fingerprint, with difficulty two and the actual early gas/state schedule. The older `private-native-v1` profile stays available for its execution-only corpora. Those old early transactions intentionally bypass header policy and are not header-valid chain evidence.

`PrivateBranch.open_admission` and `execute_admission_candidate` now compose full-size native work, computed order, parent entropy/difficulty, zone header policy, body/inclusion commitments, parent manifest, PCRC and atomic execution/root/fee checks before staging a checkpoint. Reopen/selection reuse the existing complete state/receipt/history journal. The execution-only method is refused in an admission-profile store; reopening it under the older profile is also refused.

The supported segment contains only local zone blocks before transaction activation, with no workshares or dominant blocks. Actual Go `CollectNewlyConfirmedEtxs` yields no new inbounds for that segment. The Rust entrypoint derives that empty list and accepts no caller-supplied inbound vector. Dominant/higher-order confirmation requires the still-unavailable hierarchy path; it is not replaced with an assumed empty list. Time validation uses the explicit trusted local clock supplied by the caller and runs even when proof work is cached.

## Differential qualification

The new oracle uses actual Go `CalcOrder`, `AppendHeader`, `ValidateBody`, `StateProcessor.Process`, `ValidateState`, PCRC, manifest calculation and zone ETX collection. Both engine slots use real ProgPoW, without an injected order cache or fake engine. Offline nonce searches choose local-zone order. Two 16-block branches share their first eight blocks: **24 unique blocks across 32 rows**. Header/body/state validity, native work hashes, state commitments, receipts, manifests and termini match. Explicit private branch switching and reopen pass.

Another **31 field mutations have valid native proof-of-work** and are rejected by Go and Rust. They cover clock, difficulty, limits/counters, number, parent entropy/deltas, expansion, base fee/prime terminus, manifest, coinbase scope/ledger, lock/data, state/receipt/outbound commitments, account count, uncled entropy, fee feedback and missing parent. Each is round-tripped through Go Protobuf before Go validation. Rust requires rejection without adding an object or changing HEAD.

[Admission checks](results/admission-20261112T155653219814Z/summary.json) record **284 comparisons**, including bypass/profile and clock controls. The complete default and Go-denied builds pass **183 tests**, with **149 Rust sources and 21 source assets**, zero Go/protoc invocations in the reduced build, and no Go runtime. Existing components plus these checks total **41,084 comparisons**, not a unique-coverage or completion percentage. Frozen RPC/SDK compatibility passes **70/16** checks. [Combined source-bound report](results/admission-work-20261112T160354321930Z/summary.json) and [reduced build](results/admission-work-20261112T160354321930Z/rust-no-go-build.json) retain the exact evidence.

## Measured proof reuse

A bounded memo now reuses only successful **full header proofs**, separately from the existing inclusion-work memo. Full normalized header bytes bind every field and auxiliary signature; quorum keys are immutable per verifier. Failed work never enters the memo, and a weaker inclusion certificate cannot populate it. All header/clock/body/history checks remain separate and rerun. Mutation, weak-target and capacity tests pass.

Five alternating paired rounds processed one sequence of **16 distinct headers** with native header/body/manifest/PCRC preparation and history/index insertion. Previously authenticated parents and duplicate candidate checks reuse full proofs. Median latency was **16.660309 ms → 6.556700 ms per header**, **2.54x faster**. Input decoding and epoch construction are outside timing; no execution, disk, RPC or network speedup is established. Work hashes, final header and manifests agree in both sources.

[Raw samples, contracts and source/binary hashes](results/admission-performance-20261112T155728377287Z/summary.json) and [complete baseline source](results/admission-performance-20261112T155728377287Z/baseline-source.tar.gz) are preserved. The new memo is limited to 512 entries/4 MiB logical keys and entries per verifier, separately from the inclusion memo's 4 MiB. Container/allocator overhead and epoch caches are additional. No allocation, retained-memory or RSS reduction was measured. Earlier state/cache-hit/journal measurements remain separately source-bound in the [performance ledger](PERFORMANCE_LEDGER.md).

## Remaining gates

This is a qualified local segment with trusted local ancestry, not complete network header/hierarchy or full-block authority. `Verifier.validate_block` still returns Unsupported, candidate results retain `fully_validated: false`, and history supplied to standalone component APIs remains unverified. Prime/region and coincident admission, workshares and all fork domains, confirmed rollups/inbound eligibility, mixed transaction histories, a sealed full admitted-block type, production incremental state/indexes and PoEM canonical publication/sync remain open.

Work used offline fixtures and private temporary journals. Reproduction after building:

    python3 scripts/verify_admission.py
    cargo test --locked --offline -p quai-consensus --test admission
    python3 scripts/verify_rust_build.py

Explicit Go regeneration is `scripts/gen_admission_vectors.py --go-quai ../go-quai --check`; normal builds/tests/runtime never invoke it. Installation, rollback and readable content/metadata inspection are recorded in `results/admission-installation.json`.
