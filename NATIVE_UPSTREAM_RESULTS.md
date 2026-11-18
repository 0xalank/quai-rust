# Official v0.57.0 / Polaris source migration

The compatibility baseline is official upstream commit
[`33db82f2891b267ba898ca38bd2dd8bf75cbb072`](https://github.com/dominant-strategies/go-quai/commit/33db82f2891b267ba898ca38bd2dd8bf75cbb072),
VERSION **v0.57.0**, including merged
[Polaris PR #2757](https://github.com/dominant-strategies/go-quai/pull/2757) and
[founder-forfeiture PR #2753](https://github.com/dominant-strategies/go-quai/pull/2753).
The user selected the official upstream profile. The prior five local fee
commits remain at `db42af74d31f25390236ffeca14cca2cba7de639` on
`backup/pre-polaris-fees-20261110`; they are inactive in this source profile.
Untracked Go-workspace files are preserved. No commits were pushed.

The source VERSION is v0.57.0; the last published release observed during this
migration was v0.56.1. This is an exact source pin, not a claim that a v0.57.0
release binary/tag was published. The update contains 35 upstream commits since
the v0.56.1 release, touching 69 paths. Upstream source metadata and the preserved
local profile are recorded in [reference baseline](compatibility/reference-baseline.json).

## Native changes and explicit gaps

| Upstream behavior | Native scope after this update |
| --- | --- |
| Body-header and auxiliary seal/coinbase/donor binding before order lookup, including zero-height objects | Implemented in bounded record/order/header components; invalid binding fails before order/proof reuse. |
| `PowIdentity` based on donor-header bytes after KAWPOW | Implemented. Alternate signature, AuxPow2 encoding or compatible SHA identifier cannot create new donor work. |
| Duplicate workshare and ancestor-work identity checks from Polaris | Implemented in bounded inclusion policy at Prime terminus block **2,444,574**. |
| Duplicate Qi inputs rejected before UTXO reads | Implemented in the input helper; an explicit regression checks that no lookup occurs. |
| Pool outputs require exactly 20 address bytes | The existing bounded native decoder requires 20 bytes. Historical oversized-address replay remains outside its supported domain. |
| Founder forfeiture list and activation | Asset/configuration changes are inventoried and hashed. Allocation/economic replay remains unimplemented. |
| EOA self-destruct nonce retention, lockup rollback after transaction/budget errors | Full transaction/block execution is unimplemented; no old transition is exposed as a valid Polaris block transition. |
| Live RPC request/read limits and caching changes | Full live RPC remains open. The frozen subset is revalidated under pinned RPC/SDK cases; broad live query-limit equivalence is not established. |
| Prime-sync recovery, peer fallback/stream/queue hardening and mining-job isolation | Full native transport/sync, generation workers and runtime event integration remain open. Existing application-wire helpers are checked separately. |

All full-admission, canonical-history and network-readiness flags remain false.
The update does not turn the frozen-view RPC prototype into a validating or
synchronizing node. A production target still needs G0 configuration/fork/state
qualification and the execution, storage and network gates.

## Evidence and baseline integrity

The actual v0.57.0 Go CLI builds successfully. Thirty-seven focused upstream
security tests pass. The separate offline oracle regenerates protocol,
transaction, all five schema bindings, foundation/VM, consensus/auxiliary,
history/header/hierarchy, placement/genesis, frontier/eligibility/coordinator
fixtures from the exact clean source checkout. Source hashes and revisions are
recorded beside each corpus; old fixture data was not merely relabeled.

New binding checks exposed unbound synthetic parents in several old oracle
adapters. Those adapters now construct valid body/coinbase/donor envelopes
before invoking the actual Go methods. Controlled arithmetic remains explicitly
separate from real proof cases. No source verification was disabled. The new
Polaris oracle observes actual binding, controlled order, donor identity and
inclusion-error classification at the fork boundary; it does not claim complete
validity for controlled inclusion fixtures.

The 198 new comparisons cover 50 binding/order inputs, 28 proof-identity/header
encodings and 12 inclusion observations. The existing coordinator increment is
retained and revalidated against the new baseline; its source/profile evidence
is separate from the earlier completed pre-Polaris run. Previous result
artifacts retain their original revision and meaning. Earlier performance
numbers are historical and are not measurements of this update.

Current results: [Polaris checks](results/latest-polaris.json),
[coordinator checks](results/latest-coordinator.json),
[validation ledger](results/native-validation.json), and
[coverage map](PORTING_MAP.md).

```sh
python3 scripts/build.py
python3 scripts/verify_polaris.py
python3 scripts/verify_coordinator.py
python3 scripts/verify_rust_build.py
```

The Go oracle is an explicit offline step only. The default Rust build,
distribution and runtime retain no Go dependency. Both shipped binary source
targets and every Rust source file are checked in the reduced-source build.
Future upstream updates require a new reviewed source/corpus baseline.


Final validation: **144 Rust tests** pass in both the default and reduced
Go/gofmt/protoc-blocked builds. All 102 Rust source files and both binary source
targets are present. The 198 Polaris comparisons and all prior component
regressions pass on the same source build. Coordinator comparisons pass 42,731;
RPC/SDK pass 70/16, with three raw Go/Rust loopback exchanges. A separate direct
latest-Go check passes 16 SDK comparisons. All 13 component oracle corpora reproduce
exactly. Full consensus and network readiness remain false. No performance
comparison was made.

Changed readable text, executable strings and readable extended metadata were
inspected. No AI-identifying marks were found or removed; unknown invisible
watermarks cannot be excluded by these inspections. Copyright, license
attribution and technical metadata are preserved.
