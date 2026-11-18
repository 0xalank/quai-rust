# Native genesis and candidate branches

Implemented 2026-11-10 against pinned go-quai
`db42af74d31f25390236ffeca14cca2cba7de639`.

The consensus library now constructs bounded canonical genesis payloads from an
explicit specification, seeds imported candidate ancestry, and stores multiple
candidate branches after native component checks. It does not commit a canonical
head, select a branch, execute genesis allocations or grant verified-history
status. The latest evidence is linked by `results/latest-genesis.json`.

| Corpus | Cases | Scope |
| --- | ---: | --- |
| Actual Go Genesis.ToBlock/Commit and Slice.WriteGenesisBlock payloads | 120 | Three contexts, difficulty/nonce/time/gas/extra/allocation hashes, expansion casts; raw and decoded canonical bytes |
| Actual Go hierarchy geometry and chain additions | 256 | All uint8 geometry inputs; bounded active slices through expansion 30 and chain additions 1–30 |
| Actual Go genesis CalcDifficulty paths | 72 | Base/default genesis exceptions, locations, entropy failures and wide values |
| Actual native candidate branch insert attempts | 120 | Five profiles; two sibling branches advancing three blocks each; 30 accepted and 90 rejected |

There are **2,526 comparisons**, with zero mismatches: 2,156 isolated
construction/initialization/arithmetic checks and 370 native branch assertions.
Native branch fixtures cover prime/region before and at KAWPOW activation and
pre-KAWPOW zone candidates. They use supplied trusted synthetic genesis records,
not full replay from one of the constructed public-chain genesis configurations.
The negative attempts include forged mix hashes, missing parents and wrong
manifest/interlink commitments. All accepted branch snapshots, including their
shared root, remain available after both branches advance.

`CandidateState` owns both history and ancestry indexes. It checks record
conflicts and both independent capacity budgets before inserting either value.
Failed insertion leaves both stores unchanged; identical imported entries are
idempotent. Entries are immutable by identity, so an incomplete snapshot cannot
be enriched in place. Importing ancestry does not validate it. Successful
`prepare_and_insert` returns the existing candidate component report, with
`fully_validated` and `history_verified` false. No branch selector, disk commit,
state rollback, fork scoring or canonical head is provided.

The following source boundaries are explicit:

- `Slice.WriteGenesisBlock` stores `[genesis_hash]` in every context. The Rust
  bootstrap previously returned an empty prime manifest, using the ordinary
  manifest-advancement convention. It now seeds the singleton in prime, region
  and zone contexts. Prime interlinks repeat the genesis hash four times.
- Go `Genesis.Commit` initially stores empty termini. Slice initialization fills
  the 16 dominant and subordinate references with the genesis hash. The oracle
  records the commit-stage termini separately from the candidate seed projection.
  It invokes the actual slice manifest writer; the filled termini are a bounded
  projection of the initialization loop. Pending worker generation is not replayed.
- The empty transaction created by Go `Genesis.ToBlock` contains uninitialized
  address objects that encode as empty byte strings. Go decoding reconstructs
  them as 20-byte zero addresses. The oracle retains both raw constructor bytes
  and the Go decoded canonical payload. Rust construction matches canonical
  bytes, body/header hashes and block identity; it also canonicalizes the raw
  Go payload identically. Go database byte-format parity is not claimed.
- Starting expansion is cast from uint64 to uint8, while the all-eligible bitmap
  depends on the original value being positive. For example, 256 produces
  expansion 0 with every eligibility bit set. This behavior is reproduced as a
  construction rule, without claiming that such a network configuration is valid.
- Raw geometry is reproduced for every uint8. Active slices above expansion 30
  exceed 4-bit location indexes and are unsupported. `NewChainsAdded(0)` wraps
  its predecessor to 255 in Go; the Rust chain-addition API rejects this case.
- Trusted-genesis `TotalLogEntropy` is zero in the pinned source. Outside its
  base/default-genesis exceptions, a positive parent prime entropy makes Go
  return nil. Otherwise this path yields `2^256`, a 257-bit difficulty. All
  19 such corpus values are retained exactly in a U512 projection, and the
  bounded header path rejects them instead of truncating or clamping. Another
  19 cases exercise the invalid entropy path. An explicit default-genesis hash
  enables the source exception for a chain started at expansion greater than zero.

Genesis extra data is bounded to 4 KiB before appending its 32-byte allocation
hash; integer magnitudes are at most 256 bits. The allocation hash is included
as data, not checked against executed allocation files. The candidate stores
retain their existing 4,096-record and 32 MiB encoded/history or tracked-hash
payload budgets. These budgets are not measurements of total allocator or
resident memory.

Native branch fixtures use difficulty 2 and five-second spacing. A trial with
six-second sibling spacing lowered a small synthetic difficulty to 1; Go
`CalcOrder` then truncated the `2^256` target to a zero hash and panicked in its
logarithm calculation. Rust keeps that case unsupported. These native fixtures
qualify component compatibility within their declared domain; they do not
establish universal consensus validity or production resource limits.

Reproduce locally (Go is used only for an explicit offline oracle):

```sh
GOPROXY=off GOTOOLCHAIN=local python3 scripts/gen_genesis_vectors.py --go-quai ../go-quai --check
CARGO_NET_OFFLINE=true python3 scripts/build.py
python3 scripts/verify_genesis.py
python3 scripts/verify_rust_build.py
python3 scripts/refresh_porting_map.py --go-quai ../go-quai --check
python3 scripts/check_network_plan.py
```

Full `Verifier::validate_block` remains unsupported. The frozen RPC artifact
retains its unverified view and eight-method scope. Full genesis/expansion
qualification, allocation and account/UTXO/ETX/reward replay, cross-context
branch eligibility and PoEM scoring, fork choice, durable canonical commits,
reorg/crash recovery and native libp2p/sync remain open. No performance
measurement was made in this increment.


Both the default workspace and reduced build with Go/protoc blocked pass
**100 Rust tests**. The latter has no Go sources or checkout and executes the
complete native genesis/branch checker. The offline Go oracle reproduces
exactly with `--check`. All earlier component checks pass on the same build:
5,478 placement, 5,876 hierarchy, 6,319 zone-header, 2,115 history, 6,503 auxiliary,
6,461 consensus, 37,747 VM, 6,979 protocol and 7,490 execution helper/ETX checks.
Frozen RPC/SDK checks pass 70/16 comparisons, with 8 boundary checks and 3 raw
Go/Rust application-wire loopback exchanges. These overlapping scopes do not
form a completion percentage or a node-throughput result.

Delivery inspection found no AI-identifying marks in changed readable text,
executable strings or readable extended metadata. No marks were removed;
unknown invisible watermarks cannot be excluded by these inspections.
Copyright, license attribution and technical metadata are preserved.


Follow-on startup replay and local PoEM/coordinator projections are documented
in [frontier results](NATIVE_FRONTIER_RESULTS.md). Those checks do not add
allocation execution, global eligibility or canonical fork choice.
