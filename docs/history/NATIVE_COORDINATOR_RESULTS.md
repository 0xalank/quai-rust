# Native leader selection and pending-frontier reapplication

Implemented 2026-11-10 against official v0.57.0 go-quai
`33db82f2891b267ba898ca38bd2dd8bf75cbb072` and Go 1.24.5 ordering behavior.
This increment implements bounded library operations. RPC still serves a frozen
unverified import; full block replay and network sync are unavailable.

`RecentCandidates` composes imported candidate caches with coordinator leader
selection, cross-context constraint tracing and PCRC gates. `search_native`
recomputes proof/body bindings and entropy for every cached candidate before
ranking it, then traces native orders and gates the resulting frontier. It
rejects caller-supplied entropy or number metadata that does not match those
native calculations. Declared genesis anchors remain explicit trust inputs.
`search_projection` separately accepts controlled order/entropy tables.
Neither API establishes economic history validity or canonical chain authority.

Recent views retain at most 1,000 entries per slice and 4,096 entries globally.
Inactive locations, conflicting duplicate payloads and over-capacity imports
fail before mutation. Successful duplicate imports move to newest; reads do not
touch arrival order. Native node entropy checks the context-specific body/header
commitments before proof work. These are implementation bounds, not new network
consensus rules. Profiles/records/termini remain imported and unverified.

## Source behaviors retained

- `ComputeMapPending` compares the new entropy with the **oldest** cached node,
  even before the cache fills. A node below the current best may still enter.
- `CalculateLeaders` keeps the first arrival when choosing a per-slice maximum.
  Subsequent candidate/leader sorting follows the pinned Go unstable sort,
  including equal-entropy permutations. The native Rust sort adaptation retains
  the Go Authors copyright and complete BSD license in the crate/bundle notice.
- `PendingHeadersMap` seeds absent zone caches with the supplied default genesis.
  It tries alternate candidates after failed traces and changes `first` after
  every attempted trace, including errors. Missing required imported metadata
  returns unavailable; caller caches are preserved on such returned errors.
- After a failed PCRC edge, the Go source clears exclusions and recalculates
  leaders. Its discarded circular-shift result does not change the next leader
  list. Rust reproduces the three-attempt limit and records every failed edge;
  these are observable source retries, not an improved branch-search policy.
- All-excluded leaders can produce an empty Go node or a nil-entropy panic.
  Two oracle fixtures record these boundaries. Rust returns unavailable.
- `BuildPendingHeaders` visits up to **21** pending keys in reverse processing
  order. This bound counts every visit, including non-extendable candidates.
  Initial processing order is insertion order; each completed pass sorts it by
  entropy. FIFO eviction order remains separate and unchanged by that sort.
- Equal pending entropy is first-wins. The historical best entropy may refer to
  an evicted key, in which case no generation candidate is available.

`PendingFrontiers::reapply_projection` reproduces the pending-set operations.
`reapply_native` binds an actual native component score and additionally applies
cross-context PCRC gates. Component/data errors preserve the entire pending
cache. Neither operation starts generation workers or selects a canonical head.
The additional native gates are deliberate rejection requirements for imported
unverified data; the controlled projection remains separate.

`ReapplicationSchedule` exposes the source's seven zero-cost virtual-clock
attempt times: 0, 1, 3, 7, 15, 31 and 63 seconds, with a planned final-wait horizon
at 127 seconds. It supports cancellation and checks time overflow. It does not
sleep, run an event loop, model generation duration or start asynchronous work.

## Differential evidence

| Scope | Cases or comparisons |
| --- | ---: |
| Exact candidate ordering, sizes 0 through 1,000 and nine patterns | 135 cases |
| Controlled searches over 24 multi-context datasets | 120; 40 accepted |
| Controlled additional search attempts after PCRC failures | 160 |
| Recent-cache events, including duplicates and FIFO eviction | 2,044 |
| Controlled pending reapplication passes | 90 |
| Go empty/panic leader boundaries rejected by Rust | 2 |
| Controlled comparisons | 42,267 |
| Native-proof searches | 16; all accepted |
| Native-proof recent events | 9 |
| Native score-bound reapplication passes | 12 |
| Native comparisons | 464 |
| Total new comparisons | **42,731; zero mismatches** |

The native cases reuse the previously frozen six mined prime-coincident blocks
across two sibling branches. Each run rechecks their 18 prime/region/zone
proof/header/body/append assemblies and the older 570 native eligibility
comparisons. Those 570 reused comparisons are recorded separately and are not
included in the 42,731 new-comparison count. Search and reapplication outputs
retain `fully_validated=false` and `history_verified=false`.

The offline oracle invokes actual coordinator methods over disposable explicit
backends. Observation-only hooks record search attempts and reapplication
visits. An explicit virtual clock replaces the source sleep for the schedule
case. Generation dispatch is suppressed using the existing worker-count guard;
no production backend, public peer or wallet is used. Source manifests hash the
original Go source, derived instrumented source, exact helper files, original
eligibility corpus and relevant Go toolchain sorting sources. These are bounded
compatibility cases, not full-node coverage or performance measurements.

## Reproduction and remaining work

```sh
python3 scripts/build.py
python3 scripts/verify_coordinator.py
python3 scripts/verify_rust_build.py
```

The explicit offline oracle is separate from the default Rust build/runtime:

```sh
python3 scripts/gen_coordinator_vectors.py --go-quai ../go-quai --check
```

[Latest coordinator evidence](../../results/latest-coordinator.json),
[validation ledger](../../results/native-validation.json) and
[coverage map](../../PORTING_MAP.md) track the current source build.

Pending-header body assembly, generation workers and chain-event integration
remain open. The recent-cache global capacity is bounded; durable retention,
refresh of missing imported metadata and unbounded historical replay are not
implemented. Full transaction/EVM/Qi/ETX economic execution, genesis allocations,
historical workshare qualification, canonical branch publication, durable state,
reorg/crash recovery and native libp2p/sync remain required. G0 is open, G4 partial
and G5/G6 open. The next execution work needs a block pre-state/result interface,
nested-call journal and serial transaction/Qi/ETX replay before canonical state.


Both default and reduced builds with Go/gofmt/protoc blocked pass **144 Rust
tests**. The reduced copy contains all 102 Rust source files and both actual
binary targets, no Go source or checkout, and executes the complete coordinator
checker. Its report binds to the exact default-build source hash. The offline
Go corpus reproduces exactly with `--check`. Earlier checks pass on the same
build: 14,328 eligibility, 9,988 frontier, 2,526 genesis, 5,478 placement, 5,876
hierarchy, 6,319 zone-header, 2,115 history, 6,503 auxiliary, 6,461 consensus,
37,747 VM, 6,979 protocol and 7,490 execution-helper/ETX comparisons. RPC/SDK
checks pass 70/16 comparisons, with eight boundary checks and three raw Go/Rust
application-wire loopback exchanges. Registry dependencies remain unchanged.
These overlapping scopes are not full-node coverage or performance.

Changed readable text, executable strings and readable extended metadata were
inspected. No AI-identifying marks were found or removed; unknown invisible
watermarks cannot be excluded by these inspections. Copyright, license
attribution and technical metadata are preserved.


This increment is delivered on the official Polaris baseline; see
[NATIVE_UPSTREAM_RESULTS.md](NATIVE_UPSTREAM_RESULTS.md). The earlier completed
pre-Polaris coordinator run is historical evidence, not the active source pin.
