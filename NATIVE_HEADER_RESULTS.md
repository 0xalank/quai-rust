# Native bounded zone header policy

Implemented the next native consensus slice: zone header policy, parent entropy and expansion lookup, prime-terminus/controller binding, and the reward/conversion arithmetic used by Quai's base fee. The current source passes **6,319 comparisons with zero mismatches** and **69 workspace Rust tests**. Canonical validation, full economic replay and synchronization remain open.

The offline oracle is pinned to go-quai `db42af74d31f25390236ffeca14cca2cba7de639` (VERSION v0.56.1 plus the five local fee commits). It calls the actual private `verifyHeader`, `CalcBaseFee`, `ComputeExpansionNumber` and reward/conversion functions through an explicit temporary Go overlay. The normal Rust build, runtime and distribution use no Go.

| Evidence | Cases | Result |
| --- | ---: | --- |
| Positive reward, equivalent difficulty, Qi subsidy divisor, conversion quote and base-fee arithmetic | 352 | Six values per case match Go |
| Expansion lookup, threshold window and u8 rollover | 80 | Match Go; includes zone 0,0 and 1,1 helper contexts |
| Zone header policy with controlled parent work | 3,220 | Acceptance matches actual Go `verifyHeader`; 832 accepted |
| Native parent/candidate proof and header/body components | 30 | All match Go: ten header-policy passes and five combined-component passes |

The policy/arithmetic portion contributes 6,244 comparisons. Native cases contribute 75 acceptance and authority-boundary comparisons. These counts include repeated checks of related components and are not a completion percentage or line-coverage measurement.

The controlled header cases cover fourteen prime-height profiles, five parent modes and 46 mutations/controls per mode. All three parent orders occur. Cases cover parent entropy/deltas, extra data, clock/future/uncle behavior, ordinary difficulty, prefix location rules, coinbase scope, data/lock addresses, gas/state limits and use, base fee, prime terminus, SHA/Scrypt difficulty/count/uncled fields, share targets, KAWPOW difficulty and quorum signatures. Both initial and prime-terminus exchange-rate lookup paths are exercised. Expansion cases include threshold 1,168 and wrapping expansion 255 to zero.

## Native proof and policy boundaries

`verify_zone_header_components` recomputes the parent's supported native proof/order and checks bounded zone policy against complete supplied candidate objects. It follows the source's staging: a header-policy pass does not claim the candidate's proof passed.

`inspect_zone_candidate` composes zone policy with native candidate proof, body commitments and inclusion checks. It still returns `fully_validated = false` and `history_verified = false`; economic execution and canonical replay are separate requirements. `validate_block` continues to return unsupported.

The thirty native fixtures cover a pre-KAW ProgPoW parent/child chain and signed KAWPOW candidates at activation, the depth fork, SHA-equivalent-difficulty activation and the conversion-lock fork. They use the pinned public quorum template, actual full-size native kernels and explicit trusted genesis context. Five damaged candidate proofs pass header policy and fail combined inspection, matching Go. Other mutations include base fee, parent entropy, a parent mix/signature and header data. These are constructed offline fixtures on supplied unverified ancestry, rather than live-network qualification or arbitrary historical replay.

## Arithmetic and context domain

- Base fee follows Quai's five-Qit conversion quote divided by 21,000 gas. It is not calculated from Ethereum's EIP-1559 gas-use adjustment.
- Reward quotes preserve integer rounding, Q64 logarithms, the post-KAW difficulty normalization/reset, the SHA anchor, the Qi activation/doubling schedule and conversion reward's lagged-fee selection at prime height 2,237,000.
- `economics.rs` calculates bounded positive-magnitude quotes. It does not distribute rewards, smooth/update fee state, spend UTXOs, apply conversion locks or settle economic state. Negative reset-adjusted logarithms and values exceeding the declared 256-bit output domain return unsupported.
- Required missing parent/prime/exchange-rate context returns unavailable. Imported records carry no canonical authority.
- The qualified header profile is a zone with bounded decoded fields, explicit genesis hashes and ordinary/base genesis difficulty. Expansion/default-genesis initialization outside that profile, unqualified zero-height parents, and pre-KAW nonempty workshare entropy remain unsupported. Complete prime/region header policy remains open.
- Basic ancestry checks from the prior increment remain available separately. The new frozen Go zone-header corpus supplies their broader policy qualification within this profile.

## Verification and artifacts

Current regressions pass: 69 Rust tests; 6,319 header comparisons; 2,115 history comparisons; 6,503 signed auxiliary comparisons; 6,461 consensus kernel/rule comparisons; 37,747 VM comparisons; 6,979 protocol comparisons; 7,490 execution-helper/ETX comparisons; 70 RPC cases, 16 SDK cases, eight standalone boundaries and three Go–Rust loopback exchanges. The Go-denied reduced-source build repeats the workspace tests and full native consensus, auxiliary, history and header checkers without a Go checkout or Go/protoc commands.

Build tooling now requires both executable source targets before copying binaries and verifies that the reduced build contains every Rust source file.

The shipping bundle remains two native binaries plus capabilities, notices, license and checksums. Existing frozen corpora are preserved. No performance gain or live-network readiness was measured this round.

[Latest header run](results/latest-header.json) · [Frozen source manifest](fixtures/protocol/header-source.json) · [Native validation](results/native-validation.json) · [Go-denied build](results/rust-no-go-build.json) · [Coverage map](PORTING_MAP.md)

Reproduce native checks:

```sh
CARGO_NET_OFFLINE=true python3 scripts/build.py
python3 scripts/verify_header.py
python3 scripts/verify_rust_build.py
```

Reproduce the explicit offline oracle:

```sh
GOPROXY=off GOTOOLCHAIN=local python3 scripts/gen_header_vectors.py --go-quai ../go-quai --check
```

Next consensus work: complete prime/region and expansion/genesis policy, historical workshare economics and reward distribution, economic state/receipt replay, PoEM competing-branch selection, durable validated publication/rollback and native libp2p synchronization.
