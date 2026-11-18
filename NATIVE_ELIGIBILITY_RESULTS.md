# Native coordinator eligibility components

Implemented 2026-11-10 against pinned go-quai
`db42af74d31f25390236ffeca14cca2cba7de639` (v0.56.1 plus five local fee commits).
This is a bounded library increment toward full consensus, not a validating node.

`CoordinatorView` holds explicit prime/region/zone profiles and imported records
with optional termini. Entries are immutable by context and header ID. Duplicate
imports are idempotent; conflicting imports and resource exhaustion leave the
view unchanged. A missing-termini entry is not upgraded in place; rebuild the
view when new metadata arrives. Retention/eviction and incremental fetch are not
runtime integrated. The view shares a 4,096-entry / 32 MiB tracked encoded/reference
payload budget across contexts, with at most 273 slice profiles and 10,000-step
ancestry/constraint walks. Tracked payload bytes exclude allocator overhead.

The library ports coordinator `IsAncestor`, `calculateFrontierPoints`, constraint
completion and previous-coincident-reference checks. Missing required data
returns unavailable; cycles and conflicting legacy constraints are rejected.
`trace_projection` uses explicitly supplied orders. `trace_native` computes order
through the native verifier. `Frontier::updated_with_eligibility` returns a new
frontier only after score-bound extension and cross-context PCRC checks; it does
not mutate the original frontier or publish canonical state.

Explicit legacy name aliasing remains available for source compatibility: Go
region names above the third index collide. The registered storage coordinate
is canonical while PCRC uses the physical subordinate index. Strict numeric
views reject expansions requiring ambiguous Go region names. No source anomaly
is promoted into a new network consensus rule.

## Differential evidence

| Scope | Cases or comparisons |
| --- | ---: |
| Controlled ancestry queries over 12 two-branch datasets | 3,960 cases |
| Controlled PCRC checks, including absent termini | 648 cases |
| Controlled constraint traces, including conflicting branches | 1,512 cases |
| Source missing-prime-header stale-nil-error boundaries | 3 cases |
| Policy field/outcome/immutability comparisons | 13,758 |
| Native mined prime-coincident blocks across two sibling branches | 6 blocks |
| Native proof/header/body/append/PCRC assemblies | 18 context bundles |
| Native frontier combinations | 343; 7 eligible |
| Native constraint traces | 98; 62 accepted |
| Native comparisons | 570 |
| Total new comparisons | **14,328; zero mismatches** |

The native oracle checks each mined block in prime, region and zone contexts.
Context-specific body views retain the same header and ID: zone manifests and
interlinks are cleared before both Go sanity/body validation and Rust checks.
Each successful assembly compares the returned dominant reference and persisted
next manifest/interlinks/termini; the native prime score then drives an immutable
frontier update checked against all three imported views. The matrix mixes all
seven available IDs across three frontier slots. Eligibility is reference
consistency under imported metadata, not verified economic history.

Controlled hash/order cases exercise exact Go coordinator methods through
explicit disposable backends. They are separate from six real native-proof
blocks. Both kinds of fixtures are offline and deterministic. Ancestry walks
continue through available imported predecessors across declared expansion
anchors. They stop at an anchor only when its predecessor is absent, after
comparing the parent hash; placeholder parents need not be zero. Missing
non-anchor middle records remain unavailable. A targeted Rust regression covers
an ancestor reached through an imported predecessor before the declared anchor.

The source may mutate its caller's constraint map before returning its original
copy on error. Rust preserves caller input on every path. Three fixtures capture
another source boundary: a missing prime header can return a stale nil error.
Rust rejects the missing data. These three strict rejection checks intentionally
do not treat the source's success boolean as sufficient qualification.

## Reproduction

```sh
python3 scripts/build.py
python3 scripts/verify_eligibility.py
python3 scripts/verify_rust_build.py
```

The separate offline oracle requires an explicit pinned Go checkout:

```sh
python3 scripts/gen_eligibility_vectors.py --go-quai ../go-quai --check
```

The default Rust build, shipped binaries and eligibility checker require no Go
runtime. Source manifests pin every oracle/helper and relevant Go source hash.
Latest machine evidence: [eligibility](results/latest-eligibility.json),
[validation ledger](results/native-validation.json) and
[coverage map](PORTING_MAP.md).

## Remaining consensus work

All eligibility reports retain `fully_validated=false` and
`history_verified=false`. Imported termini and anchors are not canonical proof.
Global pending-frontier leader selection, ancestor candidate search, retries,
branch reapplication and eviction integration remain open. Complete historical
workshare qualification, genesis allocations/expansion economic state, rewards,
transaction/Qi/ETX execution, durable atomic canonical commits, reorg/crash
recovery and native libp2p/sync remain required. RPC still serves a frozen
unverified import. G0 remains open, G4 partial and G5/G6 open. This round measures
compatibility and rejection behavior; it makes no performance or node-TPS claim.


Both default and reduced builds with Go/protoc blocked pass **128 Rust tests**.
The reduced copy contains every Rust source and both actual binary targets, no
Go source or checkout, and executes the complete native eligibility checker.
The offline Go corpus reproduces exactly with `--check`. Earlier checks pass
on this same source build: 9,988 frontier, 2,526 genesis, 5,478 placement, 5,876
hierarchy, 6,319 zone-header, 2,115 history, 6,503 auxiliary, 6,461 consensus,
37,747 VM, 6,979 protocol and 7,490 execution-helper/ETX comparisons. RPC/SDK
checks pass 70/16 comparisons, with eight boundary checks and three raw Go/Rust
application-wire loopback exchanges. Registry dependencies remain unchanged.
These overlapping scopes are not full-node coverage or performance.

Changed readable text, executable strings and readable extended metadata were
inspected. No AI-identifying marks were found or removed; unknown invisible
watermarks cannot be excluded by these inspections. Copyright, license
attribution and technical metadata are preserved.
