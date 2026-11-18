# Native candidate ancestry and workshare inclusion

Implemented a bounded native Rust zone-history and inclusion slice against go-quai `db42af74d31f25390236ffeca14cca2cba7de639` (VERSION v0.56.1, including the five local fee commits). The current run passes **2,115 comparisons with zero mismatches** and **62 workspace Rust tests**. Full consensus, canonical history qualification and network synchronization remain open.

| Evidence | Inputs | Result |
| --- | ---: | --- |
| SHA/Scrypt difficulty, count, uncled-count and allocation controllers | 384 | Match actual Go controller methods |
| KAWPOW share difficulty and discount arithmetic | 256 | Match actual Go arithmetic |
| KAWPOW difficulty EMA and liveness | 144 | Match actual Go arithmetic, including u32 time wrapping |
| Workshare classification | 360 | Match Go policy; controlled work hashes for kernel-backed cases and native donor hashes for SHA/Scrypt cases |
| Complete candidate zone-history distances | 49 | Match Go distance acceptance and accepted values; 16 accepted |
| Pre-KAW inclusion policy | 66 | Match actual `VerifyUncles` with a test-only controlled-hash engine; 20 accepted |
| Native signed KAWPOW inclusion | 48 | Match actual `VerifyUncles` using the real KAWPOW engine; four accepted and 44 rejected |

The first six rows contribute 2,063 comparisons. Native inclusion contributes 52: 48 acceptance comparisons plus four checks that accepted reports grant no canonical authority. Repeated checks and overlapping components are not additional feature coverage.

The four native fixtures cover KAWPOW activation (1,171,500), the inclusion-depth change (1,330,000), Singularity (1,530,500) and conversion/controller stability (1,872,600). Each uses the pinned public quorum template and recomputed full-size KAWPOW work. Mutations cover duplicate inclusion, SHA count, Scrypt difficulty, number, prime terminus, early lock, empty data, signature, mix, donor Merkle root and signature time. These are constructed offline fixtures with an explicitly trusted genesis parent and supplied candidate ancestry. They do not establish correctness on arbitrary historical branches or on live network data.

## Native implementation

- `share_rules.rs` implements bounded positive-magnitude controller arithmetic, signed floor rounding, EMA periods, forked count/depth limits, discount rules and classification. SHA/Scrypt target comparison is strict. Zero/negative compact targets, invalid controller inputs and arithmetic outside the declared domain return explicit errors.
- `history.rs` stores complete decoded candidate objects by header ID, with atomic duplicate/conflict handling, a 4,096-record limit and a 32 MiB canonical encoded-byte budget. This budget measures encoded records, not total retained heap. History carries no validated/canonical status.
- `validation.rs` checks duplicate and ancestor inclusion, dangling parents, distance, coinbase/data/lock rules, proof role, per-algorithm count limits, ordinary difficulty, prime terminus and SHA/Scrypt controller fields. Its runtime entry point recomputes work through the native verifier and checks body commitments. The controlled-hash projection is a separate API and cannot construct native verified-work facts.
- Missing required ancestry returns `Unavailable`. The reference's ordinary difficulty fallback to the parent's difficulty when the grandparent is absent is preserved separately. Nonmonotonic or overflowed distance histories are outside the qualified domain.
- The historical signature exception is restricted to inclusion of out-of-scope SHA/Scrypt coinbases, derived from node location. Other auxiliary bindings remain required. Block admission continues to require quorum signatures. A focused test covers this boundary.
- Basic header ancestry components now check parent/time/number, body-header commitment, zone location, coinbase/data policy and gas/state-limit schedules. Their focused native tests are separate from the Go inclusion corpus; they do not qualify full `verifyHeader` equivalence.

## Current verification

The rebuilt source passes 62 Rust tests, 2,115 history comparisons, 6,503 signed auxiliary comparisons, 6,461 consensus-kernel/rule comparisons, 37,747 VM comparisons, 6,979 protocol comparisons, 7,490 execution-helper/ETX comparisons, 70 frozen RPC cases, 16 SDK cases and eight standalone boundary checks. Three Go–Rust loopback exchanges also pass. Scope and counts are recorded separately in the result summaries.

The reduced-source build additionally denies `go`, `gofmt` and `protoc`, excludes the Go checkout, and executes the full native consensus, auxiliary and history checkers. The distribution remains the two native executables and four supporting files. Go oracle sources remain in the explicit reference profile.

Raw evidence: [history run](results/latest-history.json), [source manifest](fixtures/protocol/history-source.json), [native validation](results/native-validation.json), [Go-denied build](results/rust-no-go-build.json), and [coverage map](PORTING_MAP.md).

Reproduce the native slice:

```sh
CARGO_NET_OFFLINE=true python3 scripts/build.py
python3 scripts/verify_history.py
python3 scripts/verify_rust_build.py
```

Reproduce the separately invoked offline oracle:

```sh
GOPROXY=off GOTOOLCHAIN=local python3 scripts/gen_history_vectors.py --go-quai ../go-quai --check
```

## Remaining consensus work

Full header entropy, base fee, expansion/genesis initialization, prime/region policy and eligibility; full historical/pre-fork workshare economics and rewards; cross-context history; PoEM competing-branch selection; complete economic state/receipt replay; durable validated publication and rollback; and native libp2p synchronization remain required. `validate_block` still returns unsupported, and accepted component reports keep `fully_validated = false` and `history_verified = false`. No speed improvement or live network readiness was measured in this round.
