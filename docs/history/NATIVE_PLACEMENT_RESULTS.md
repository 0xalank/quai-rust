# Native candidate append components

Implemented 2026-11-10 against pinned go-quai
`db42af74d31f25390236ffeca14cca2cba7de639`.

The Rust consensus library now prepares bounded candidate append transitions in
prime, region and zone contexts. It recomputes candidate proof/order/rank,
selects the append route, composes the existing native header/body checks,
checks local parent manifest or prime interlink commitments, and computes PCRC
(previous coincident reference), manifest and interlink updates.
The parent local manifest uses slot `node_context`; the subordinate manifest
inside the body uses a separate subordinate-context commitment. These are
checked at distinct stages.
The API returns a candidate value and leaves supplied history and indexes
unchanged. It does not persist or publish a canonical branch.

The final verification evidence is linked by `results/latest-placement.json`.

| Corpus | Cases | Accepted | Purpose |
| --- | ---: | ---: | --- |
| Controlled order/rank and actual Go append-queue routing | 336 | all 3 orders, all 5 ranks | Isolated arithmetic/routing projection; supplied work hashes |
| Stored manifest and interlink advancement | 336 | 336 | Exact persisted values; interlinks where applicable |
| Local parent append commitments | 72 | 16 | Isolated actual Go AppendHeader ancestry path; other header policy bypassed explicitly |
| Termini reconstruction/canonicalization | 48 | 36 | Fixed 16 dominant and 16 subordinate entries, bounded short hashes |
| Actual Go Slice PCRC transitions and batch contents | 756 | 630 | Distinct node/header indexes, cycles, dominant origin and genesis exceptions |
| Native proof/header/body plus append preparation | 56 | 13 | Composed positive and negative native cases; no fake proof mode |

The complete corpus performs **5,478 comparisons**: 5,301 isolated policy/codec
comparisons and 177 native preparation assertions. Native accepted cases cover
prime/region before and at KAWPOW activation and a pre-KAWPOW zone profile,
including dominant-origin requests. Post-KAWPOW zone append preparation,
competing branches, nonempty economic bodies and expansion initialization are
outside this new composed corpus. Earlier zone/header/inclusion corpora remain
separate evidence; they are not additional accepted append fixtures here.

The native negatives include a forged mix hash, wrong or missing parent
manifest/interlinks, a cyclic dominant reference, stale timestamp and bad header
state/base-fee fields. The test suite also verifies that importing a candidate
cannot bypass header policy or the computed append route. Some failures may be
rejected at an earlier component than Go; no first-error equivalence is claimed.

`placement::CandidateIndex` limits records to 4,096, each manifest to 4,096
hashes, and tracked hash/option payload to 32 MiB. This counter is a resource
budget, not a measurement of total allocator or resident memory. Duplicate
identical snapshots are idempotent; conflicting or oversized inserts fail
without changing the index. Termini protobuf input is bounded at 8 KiB and
supports hashes of at most 32 bytes. Hashes outside the qualified domain are
unsupported. Prime interlink arrays have exactly four entries.

Two source details matter for compatibility:

- On an uncached first call, Go `CalculateManifest` writes the computed manifest
  but returns nil because its local variable shadows the outer variable. The
  oracle records `returned_manifest` separately and compares Rust's next
  candidate manifest to the actual rawdb value. Go source is unchanged.
- Go rawdb reconstructs a stored empty manifest as nil. Region/zone AppendHeader
  treats that value as missing; Rust rejects it as unavailable. Ordinary prime manifest
  advancement yields an empty list. Genesis seeding is separate; its singleton
  manifest is corrected and qualified in [genesis results](NATIVE_GENESIS_RESULTS.md).

The native oracle re-decodes objects between proof-mining attempts, including
cache hits, so Go per-object cached work atomics cannot supply stale proof data.
Controlled order and cached-header commitment projections are clearly separate
from native engine verification. No Go runtime dependency or fake-proof mode is
included in the default Rust build.

Run from the project directory (Go is only used for explicit oracle refresh):

```sh
GOPROXY=off GOTOOLCHAIN=local python3 scripts/gen_placement_vectors.py --go-quai ../go-quai --check
CARGO_NET_OFFLINE=true python3 scripts/build.py
python3 scripts/verify_placement.py
python3 scripts/verify_rust_build.py
python3 scripts/refresh_porting_map.py --go-quai ../go-quai --check
python3 scripts/check_network_plan.py
```

`Prepared.fully_validated` and `history_verified` remain false. Full
`Verifier::validate_block` still returns unsupported. The bounded candidate
library capability is distinct from full network placement qualification,
which remains false. No RPC import trust gate or canonical database is wired
to these candidate reports.

Next work: qualify genesis/expansion initialization and full cross-context
ancestry/eligibility with multi-block competing branches; implement economic
account/UTXO/ETX and reward replay; then fork choice, atomic durable commits and
reorg/crash recovery. Native libp2p transport, sync and full transaction execution
remain open. This round establishes compatibility evidence, not a speed or node
throughput result. See [coverage map](../../PORTING_MAP.md) and
[network gates](../../NETWORK_READINESS.md).


The final default workspace and reduced Go-denied builds each pass **88 Rust
tests**. The placement oracle reproduces exactly with `--check`; the new corpus
has **zero mismatches**. Existing checks also pass: 5,876 prime/region hierarchy,
6,319 zone-header, 2,115 history/inclusion, 6,503 auxiliary, 6,461 consensus,
37,747 VM, 6,979 protocol and 7,490 execution helper/ETX checks. Frozen RPC and
SDK checks pass 70 and 16 comparisons, with 8 standalone rejection/boundary
checks and 3 raw application-wire Go/Rust loopback exchanges. These categories
measure overlapping bounded components and must not be summed into a coverage
percentage or a network-readiness claim.

Delivery inspection covers changed readable text, executable strings and
readable extended attributes. No AI-identifying marks were found or removed;
unknown invisible watermarks cannot be excluded by these inspections. Copyright,
license notices, protocol provenance and technical metadata are preserved.
