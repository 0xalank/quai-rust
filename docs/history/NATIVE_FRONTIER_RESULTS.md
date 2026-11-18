# Native startup, PoEM scores and coordinator projections

Implemented 2026-11-10 against pinned go-quai
`db42af74d31f25390236ffeca14cca2cba7de639`.

The Rust library now scores supported native candidate components, compares
scores within an identical supplied chain profile, and inserts candidate data
only after scoring succeeds. A separate frontier type projects the coordinator's
prime/region/zone tip set, coincident-parent requirements, updates and summed
entropy. Pending frontiers reproduce entropy-key lookup, first-wins collisions,
oldest-key eviction and the coordinator's historical best-entropy watermark.
None of these operations selects or commits a canonical chain.

Evidence is linked by `results/latest-frontier.json`.

| Corpus | Cases | Qualified behavior |
| --- | ---: | --- |
| Actual Go controlled-work PoEM entropy metrics | 168 | All contexts; pre-KAWPOW empty shares and post-KAWPOW count arithmetic through 32 |
| Actual Go NodeSet Extendable/Update/Copy/Entropy | 192 | Prime/region/zone updates, parent mismatches, untouched copies, wide sums and legacy region aliases |
| Actual Go coordinator Add/Get pending behavior | 2,005 offers | Equal-entropy first-wins behavior, 2,000-entry capacity, no-touch lookup and an evicted historical best |
| Native proof/header/body/append/score attempts | 240 | Nine branch profiles, 54 accepted and 186 rejected; two sibling branches, three blocks each |
| Actual coordinator updates using native candidate scores | 54 | Updated tips and complete frontier entropy after each accepted candidate |

There are **9,988 comparisons**, with zero mismatches: 9,054 isolated
arithmetic/coordinator assertions and 934 native startup/score/frontier assertions.
These categories overlap and do not constitute a percentage of client completion.

Four native profiles start from actual Go `Genesis.Commit` blocks at expansions
1, 2, 4 and 30, rather than supplied synthetic anchors. Rust constructs the same
canonical root, receives the explicit default-genesis identity, seeds candidate
ancestry and advances six zone candidates per profile. This closes a bounded
startup component test; genesis allocation execution and public-network startup
remain unqualified. The other five profiles cover prime/region before and at
KAWPOW activation plus a pre-KAWPOW zone profile. Coincident parent slots are
coupled explicitly so the native candidates satisfy the coordinator projection,
in addition to local header/append policy.

Native rejection cases include forged mix hashes, missing parents, wrong
manifest/interlink commitments, incorrect default-genesis configuration at the
first child and nonconsecutive block numbers later in the startup sequence.
Failed scoring/insertion leaves both candidate stores unchanged. Both sibling
branches and their common anchor remain available after replay.

`CandidateScore` has private binding fields and is produced by the supported
native component path. A score comparison rejects different chain profiles,
including location, genesis identities, default genesis and numeric parameters.
Declared zero-height genesis anchors have a zero score and no checked work;
they cannot authorize frontier updates. Incoming non-genesis candidates are
rechecked even when already imported. Imported ancestry remains unverified, and
all full-validation/history-authority reports remain false.

A successful local score is insufficient to establish a complete frontier.
`Frontier` requires every active prime, region and zone entry, checks the parent
slots required by the computed order, and updates only those coincident contexts.
It sums bounded 256-bit tip values in U512 so sums above 256 bits are retained.
`updated_with_score` binds the record identity, checked work, local order and
expansion. Global PCRC/ancestor/leader constraints, pending-map search,
reapplication and canonical fork choice are not implemented here.

A pinned-source naming boundary is explicit. `common.Location.Name()` calls
regions numbered 3 and above `unknownregion`; once five or more regions exist,
distinct region and zone coordinates share coordinator map keys. Numeric location
validity alone does not prevent these aliases. The default frontier constructor
rejects these ambiguous layouts. `from_legacy_imported` is an explicit source
compatibility view: aliased entries must agree, updates propagate across the
alias group and summed entropy reproduces Go's repeated map references. The
legacy projection supports the bounded 4-bit geometry through expansion 30;
its positive results do not qualify those layouts for independent network use.
The expansion-30 native coordinator assertions use this explicit legacy view.

The source pending cache is keyed by entropy, rather than a frontier identity.
The first frontier at an existing key is retained. Add/Get use no-touch lookups,
so eviction follows insertion order. The `best_ever` watermark can refer to an
entry that has been evicted; it is not a retained-best selector or a chain head.
Rust pending capacity is bounded at 2,000 entries. Complete frontiers can contain
273 numeric tip slots; this is a count bound, not a measured heap/RSS guarantee.

Nonempty pre-KAWPOW discounted workshare entropy remains unsupported. The
post-KAWPOW 32-share arithmetic cases are controlled projections; a count-based
entropy calculation alone does not pass the fork-dependent inclusion limits.
Native profiles in this corpus use empty economic bodies/shares. Full transaction,
account/UTXO/ETX/reward and receipt replay remain separate gates. No speed or node
throughput measurement was made in this increment.

Reproduce locally (Go is an explicit offline reference only):

```sh
GOPROXY=off GOTOOLCHAIN=local python3 scripts/gen_frontier_vectors.py --go-quai ../go-quai --check
CARGO_NET_OFFLINE=true python3 scripts/build.py
python3 scripts/verify_frontier.py
python3 scripts/verify_rust_build.py
python3 scripts/refresh_porting_map.py --go-quai ../go-quai --check
python3 scripts/check_network_plan.py
```

The shipping artifact remains a frozen, unverified RPC view. Full
`Verifier::validate_block` still returns unsupported. Allocation/economic replay,
global coordinator eligibility and search, canonical fork choice, atomic durable
state, reorg/crash recovery and native libp2p/sync remain open.


Both default and reduced builds with Go/protoc blocked pass **116 Rust tests**.
The reduced copy contains no Go sources or checkout and runs the complete
native startup/PoEM/frontier checker. The offline Go corpus reproduces exactly
with `--check`. All earlier checks pass on the same source build: 2,526 genesis,
5,478 placement, 5,876 hierarchy, 6,319 zone-header, 2,115 history, 6,503 auxiliary,
6,461 consensus, 37,747 VM, 6,979 protocol and 7,490 execution helper/ETX checks.
Frozen RPC/SDK regressions pass 70/16 comparisons, with 8 boundary checks and
3 Go/Rust raw application-wire loopback exchanges. Registry dependencies remain
unchanged. These overlapping scopes are not full-node coverage or performance.

Same text as in [NATIVE_COORDINATOR_RESULTS.md](NATIVE_COORDINATOR_RESULTS.md); omitted here.