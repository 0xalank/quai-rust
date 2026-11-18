# Native prime and region header policy

Implemented a bounded Rust prime/region header slice against go-quai `db42af74d31f25390236ffeca14cca2cba7de639` (VERSION v0.56.1 plus the five local fee commits). The current run passes **5,876 comparisons with zero mismatches**. Full canonical validation and economic replay remain open.

| Evidence | Cases | Result |
| --- | ---: | --- |
| Efficiency score, including 16-bit truncation/wrapping and zero-signal behavior | 384 | Match actual `ComputeEfficiencyScore` |
| Threshold window/count transitions through actual prime `verifyHeader` | 128 | 64 accepted, 64 rejected |
| Miner-difficulty activation and EMA | 128 | Match actual `ComputeMinerDifficulty`, including 256-bit maximum inputs |
| ETX eligibility, all 256 slices and activation edges | 3,072 | Match actual `UpdateEtxEligibleSlices` |
| Prime/region header policy with controlled parent work | 1,360 | Match actual `verifyHeader`; 545 accepted |
| Native parent/candidate proof and non-zone body components | 66 | 42 header-policy passes; 21 combined-component passes |

The arithmetic/policy portion contributes 5,681 comparisons. Native fixtures contribute 195 acceptance and report-boundary comparisons. Repeated checks of related components are not a completion percentage or line coverage.

## Implemented behavior

`hierarchy_header.rs` adds separate prime/region profiles, context-specific parent lookup and numbers, parent entropy/delta rules, parent-order constraints, slice-prefix matching, body-header commitment, time/extra limits and signed auxiliary binding checks. Prime policy checks efficiency, threshold count, ETX eligibility, the empty prime-state root and miner difficulty. Region policy checks the empty region-state root. Zone-specific difficulty schedules, coinbase/data/lock policy, gas/state limits, base fee and share controllers remain in the zone path, matching the source's staging.

Two details are preserved exactly: Go computes efficiency EWMA in `uint16`, wrapping multiplication and addition before division; and the current expansion trigger threshold is `uint16::MAX`, so a zero threshold count cannot start advancing through the score comparison. Existing nonzero counts still follow the window/reset/wrapping rules. ETX eligibility uses low bits within each byte and the strict `parent zone number > 259,200` boundary. Miner difficulty switches by the parent's prime-context number, with a 4,000-sample EMA.

The native entry point recomputes supported parent work/order when the parent's number in the node context is nonzero. It reports that fact explicitly. For a zero-context parent, it mirrors Go's skipped parent-work stage and distinguishes a declared genesis hash from an untrusted zero-number object when calculating entropy. Such a parent does not establish canonical/genesis authority.

The composed inspector adds native candidate work and non-zone body commitments: nonempty subordinate manifest, empty transaction/ETX/uncle lists, and the prime interlink root. Candidate hierarchical placement remains a separate gate. Accepted reports keep `fully_validated = false`, `history_verified = false` and `candidate_order_checked = false`. `validate_block` still returns unsupported.

## Fixture scope and limitations

Header-policy fixtures cover prime and region-0 profiles, five prime-height profiles, all three parent orders and 34 mutations/controls per profile. The separate bitmap helper covers every region/zone position. Prime and region acceptance/rejection is compared with the actual private Go function through an explicit temporary overlay. Controlled parent hashes qualify policy projections and cannot construct native proof facts or canonical publication tokens.

The 66 native cases use actual ProgPoW/KAWPOW kernels and the pinned public quorum template. They cover both node contexts before KAWPOW, at KAWPOW activation and at SHA-equivalent-difficulty activation. Mutations include candidate and parent mixes, entropy, signatures, empty manifests, interlinks, uncle lists and state roots. Thirty accepted header cases check real parent work; twelve accepted cases exercise zero-context parents with and without explicit genesis trust. These are constructed offline component fixtures on supplied candidate history. They do not qualify arbitrary historical replay or a live network configuration.

Decoded magnitudes are bounded to 256 bits and stored hierarchy numbers to `u64`. Malformed node locations, missing required history, clock/number overflow and a nonzero uncled-entropy signal divided by zero delta return explicit errors. The latter is outside the qualified Go arithmetic domain. Imports carry no canonical status.

## Verification

The rebuilt workspace passes **77 Rust tests**, the new 5,876 comparisons, and all existing regressions: 6,319 zone-header, 2,115 history, 6,503 auxiliary, 6,461 consensus kernel/rule, 37,747 VM, 6,979 protocol and 7,490 execution-helper/ETX comparisons; 70 RPC, 16 SDK, eight standalone boundaries and three Go–Rust loopback exchanges.

The reduced-source build requires both executable source targets, verifies every Rust source was copied, excludes the Go checkout and denies `go`, `gofmt` and `protoc`. It runs the workspace tests and full native consensus, auxiliary, history, zone-header and prime/region checkers. The shipping bundle remains two native executables and four supporting files. Existing corpora and pinned registry dependencies are preserved. No speed improvement was measured this round.

[Latest hierarchy run](results/latest-hierarchy.json) · [Source manifest](fixtures/protocol/hierarchy-source.json) · [Native validation](results/native-validation.json) · [Go-denied build](results/rust-no-go-build.json) · [Coverage map](PORTING_MAP.md)

Reproduce native checks:

```sh
CARGO_NET_OFFLINE=true python3 scripts/build.py
python3 scripts/verify_hierarchy.py
python3 scripts/verify_rust_build.py
```

Reproduce the explicitly invoked offline oracle:

```sh
GOPROXY=off GOTOOLCHAIN=local python3 scripts/gen_hierarchy_vectors.py --go-quai ../go-quai --check
```

Next: hierarchy placement, termini/interlink eligibility and expansion/genesis qualification; historical workshare economics and reward/state replay; PoEM competing branches, durable validated publication/rollback and native libp2p sync.
