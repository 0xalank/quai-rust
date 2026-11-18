# quai-rust roadmap

This is the single plan and status file for the port. Update it when work
lands. Do not add a new `*_RESULTS.md` report for each step; put the evidence
in the commit message and keep raw run output in `results/` (local, ignored).

**Measure of progress:** the highest mainnet block that Rust replays with every
root equal to go-quai v0.57.0 (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`).

Current value: **zone 11,682** (2026-11-17, `0e13a64`). `quai-node replay
--state` executes mainnet zone blocks 0–11,682 from the colosseum genesis, and
every root equals go-quai: EVM state, ETX set, UTXO, receipts, outbound ETXs,
gas and state counters, account count. 11,682 is the end of the current export,
not a mismatch. User transactions start at zone 259,200, so execution of user
transactions is not tested on mainnet yet.

## Status on 2026-11-17

- 6 crates, 37K lines of Rust source, 18K lines of tests and examples. Go has
  about 120K lines of non-test code.
- `cargo test --release --locked --workspace`: 281/281 tests in 55 suites at the
  start of the day; 328/328 in 62 suites after the day's merges (`d097a4b`,
  23 min), plus 21/21 for `quai-network --features native-transport`.
- `Verifier::validate_block` returns `Unsupported`
  (`crates/quai-consensus/src/verifier.rs`). The RPC serves a frozen snapshot.
- Before the mainnet replay, all sync tests used private chain 1337. An older
  build synced 259,216 private blocks with matching roots; only the last 16 had
  user transactions.
- Measured against Go: Qi signature checks 2.6x faster, decoding 1.8x faster,
  69–81% fewer bytes allocated; Quai ECDSA sender recovery 0.6x (slower).

| Gate | Scope | Status |
|---|---|---|
| B0 | Build without Go | Done |
| G0–G2 | Chain spec, wire formats, commitments | Partial |
| G3 | EVM, Qi and ETX execution | Partial |
| G4 | PoW, AuxPoW, workshares, fork choice | Partial |
| G5 | Durable state, reorg, crash recovery | Private chain only |
| G6–G7 | P2P and sync | Loopback only, peers added by hand |
| G8 | Mainnet shadow sync | Not started |
| G9 | Live RPC, tx pool, wallets | Snapshot reads only |
| G10–G11 | Production tests, mining | Not started |

`NETWORK_READINESS.md` keeps the detailed gate definitions.

## Phase 0: protect the work

Gate: the repo has a first commit and one status file.

- [x] Ignore `results/` (2.4 GB of run output). It stays on disk because 50
      scripts read and write it.
- [x] First commit of the tree (`59bc106`). Fixtures are plain Git: 225 MB of
      JSON packs to about 11 MB, so Git LFS would store more, not less.
- [x] This file replaces per-step reports.
- [x] Move the historical reports to `docs/history/` (53 files) and fix the
      `PORTING_MAP.md` generator, which printed "0 comparisons": the newest
      `results/native-validation.json` lost 30 keys. The map now shows the last
      recorded values and names the record they come from. Regenerating the map
      needs the local, ignored `results/` directory.

## Phase 1: mainnet replay (critical path)

Gate: Rust replays mainnet zone 0-0 to the live head, and every root equals Go.

1. [ ] Sync a go-quai v0.57.0 node on mainnet, slice `[0 0]`:
       `scripts/mainnet/go_node.sh start|stop|status`. Data:
       `~/quai-data/mainnet-v0.57.0`. UPnP port mapping and telemetry are off.
       A disk guard stops the node below 25 GiB free.
       Limit found on 2026-11-17: mainnet zone head is about 11.2M blocks; the
       node syncs about 3.7 zone blocks/s and stores about 30 KB per zone block.
       A full sync needs about 35 days and 336 GB. On this disk the guard stops
       it near 900,000 blocks, before KawPow. Plan: use the Go node as the
       oracle for the early range, and feed replay beyond it from peers through
       `quai_network::public` (Phase 2.2 groundwork), or move the node to a
       larger volume.
2. [x] Go exporter: for each block, the prime, region and zone work objects
       (protobuf) plus receipts, in order from genesis. The header fields carry
       the expected state, UTXO, ETX and receipt roots.
       `scripts/mainnet/export.sh` (default: stop node, APFS clone, restart,
       export from the clone; `--live` clones without a stop). Risk: an export
       takes about 6.5 KB per zone height, so a full mainnet export does not fit
       next to the node on this disk. Before that point, stream segments into
       replay and delete them, compress them (zstd gives 3.8x; needs QEX2), or
       move the data to another volume.
3. [x] Rust `replay` command: start at mainnet genesis with the official
       allocations and compare every root. Stop at the first mismatch and print
       it. `quai-node replay --export DIR [--state] [--to N] [--jobs N]`; one
       result line on stdout, a JSON report in `<datadir>/replay/last.json`.
       Speed on this Mac: 35 zone blocks/s stateless, 26 with `--state` (3 PoW
       threads, idle-ish), 15.6 under load 20. Proof-of-work is about 94% of the
       time: about 8.6 ProgPoW light checks per height (block plus
       workshares). Peak memory 208 MB.
       Done (`4828e14`): `--state` runs on the lazy World, UTXO, ETX and lockup
       state in a quai-store database (new version every 1,000 heights);
       redemption and trim history in `past.redb`, pruned to 1 year / 84 days;
       a compacted checkpoint every 10,000 heights and at the end (atomic
       writes, SHA-256, roots checked against the header on `--resume`); only
       the last two checkpoints are kept. `scripts/mainnet/replay_loop.sh` runs
       one pass: live export, then `replay --resume --state`. Peak RSS about
       200 MB, flat with chain length.
       Scale limits: `past.redb` grows about 2.5 KB per height (about 16 GB
       over the 1-year window); compaction copies the whole live state in one
       transaction and needs chunking at mainnet scale.
       Next: a peer block source (`peer-export`) so that replay is not limited
       by the Go node's sync speed.
4. [ ] Fix the first mismatch, then run again. Milestones are the fork heights
       in go-quai `params/protocol_params.go`: KawPow 1,171,500, Singularity
       1,530,500, ShaEquivalentDifficulty 1,755,000, ConversionStability
       1,872,600, SelfDestructRefund 1,919,500, ConversionLockChange 2,237,000,
       SecurityHardening 2,317,000, Polaris 2,444,574 (prime terminus),
       MaxCodeSize 3,490,000.
5. [x] Remove the EVM limits that Go does not have, so that only gas limits
       execution (`130b106`). Removed: steps per frame, EVM memory, logs per
       transaction, call input, access-list size, the 1,000,000-entry undo
       journal, precompile input and modexp caps, transaction/receipt/block
       decode caps, opETX access-list caps and the lazy World overlay caps.
       With the 50,000,000 gas ceiling, memory stays under 5.1 MB per frame and
       140.6 MB over 1,025 frames (proof in `evm/mod.rs`). Kept because Go has
       the same limit: code size 32,768, BLOCKHASH depth 256, the u16 ETX index,
       the 3 MiB message cap. Two silent divergences were fixed on the way:
       opETX pushed 0 where Go succeeds on a long access list, and opETX
       required 20-byte addresses where Go accepts any length. 19 new Go
       oracle vectors; 11 new tests fail on the old code.
       Still open: change caps of 65,536 and a 32 MiB payload cap in the ETX
       queue and lockup overlays (`etx.rs`, `lockup_state.rs`), which a
       long-lived overlay can reach; header-path caps without a Go equivalent
       (64 KiB header input, 4,096-byte header data, 4,096-entry manifest;
       step 6); 8,192 ETXs per rollup. The in-memory World keeps its snapshot
       bounds (65,536 accounts), so mainnet replay must move to the lazy World
       (`restore_sparse`) before the state passes them.
6. [ ] Replace the per-profile code with one chain spec and one fork schedule.
       Then make `validate_block` do real validation.
       The replay works around private-profile entry points today: it copies
       the ETX confirmation logic of `Hierarchy::prepare_scoped` into
       `crates/quai-node/src/state.rs`. `prepare_scoped` refuses prime >=
       1,171,500, zone > 259,200 outside the private activated profiles,
       non-coinbase ETXs in the early domain, prime >= 262,000 (controllers)
       and non-genesis economic fields, and it writes zone `[0,0]` and region 0
       as literals. The mainnet spec must supply: chain ID 9, location, active
       hierarchy from the expansion number; the genesis fields; header policy
       (minimum difficulty, duration, gas and state ceilings, workshare
       thresholds, time limits, adjustment); one fork table (the heights are
       literals in rules.rs, share_rules.rs, historical.rs and the EVM today);
       AuxPoW quorum keys, allocations, lock periods, trim depths.
7. [x] Check proof-of-work in a separate parallel pass. Header proofs need no
       state. `quai-node replay` verifies block and workshare proofs on worker
       threads with shared epoch caches (`pow::SharedCaches`).

## Phase 2: node shell

Gate: the Rust node follows the live mainnet head for 2 weeks with no
divergence from Go.

1. [ ] `quai-node` crate: config file, data directory, logs, metrics, and the
       `replay` command from Phase 1. Started: the crate, `--datadir`,
       `--log-level` and `replay` exist (`0e13a64`); config file, metrics and
       `run` do not.
2. [ ] Public P2P: bootnodes, Kademlia discovery, GossipSub topics, peer scoring.
       Groundwork (`9a25090`): read-only `quai_network::public` and the
       `mainnet-probe` example work with go-quai v0.57.0 (local) and the v0.57.1
       boot peers: Noise/Yamux, identify, `/quai/kad/1.0.0`, all `/quai/1.0.0`
       request types, zone block and workshare gossip. Still needed: a relay
       client, DCUtR and AutoNAT (many peers have only `/p2p-circuit`
       addresses); the private profile's Kademlia ID is `/quai`, Go uses
       `/quai/kad/1.0.0`; Go's GossipSub message-ID function; GossipSub
       validation equal to Go before relay; request scoring and bans; per-IP
       limits; answers only from validated state.
3. [ ] Follow the live head. Send gossiped blocks and workshares through the
       same validation path.
4. [ ] Run next to a Go node and compare heads and roots.

## Phase 3: product

Gate: Quais.js and Pelagus work against the Rust node.

1. [ ] RPC on validated state: balances, UTXOs, `call`, `estimateGas`,
       receipts, logs, filters, WebSocket.
2. [ ] Tx pool and `sendRawTransaction`.
3. [ ] Fuzz every decoder. Remove `.unwrap()` from code that reads peer data
       (457 calls in non-test code on 2026-11-17).
       Started (`5df7277`): deterministic fuzz-style tests on stable Rust for
       the primitives, network, consensus and execution decoders (no panic,
       bounded allocation, re-encoding fixed point; `QUAI_FUZZ_SCALE=N` for
       longer runs). Found and fixed: frame reads that reserved the declared
       length (up to 3 MiB) before data arrived, and a hierarchy-delta
       reservation of about 2.2 MB from a few input bytes. Non-test
       `.unwrap()`: 466 to 354 in the old crates (`quai-node` adds 57). Left:
       about 150 unwraps of proto fields that `Header::decode` guarantees
       (needs API changes), `evm/*`, `quai-rpc`. Open question: `Lockup::decode`
       accepts a 58-byte record with a zero delegate that `encode` writes as
       38 bytes; check Go's behaviour.

## Phase 4: speed

Gate: mainnet replay is at least as fast as Go and uses less memory.

1. [x] libsecp256k1 (the `secp256k1` crate) for ECDSA sender recovery
       (`secp256k1 =0.33.1`, `crates/quai-primitives/src/recover.rs`,
       `5df7277`). Equal to k256 on 165,445 raw cases and 30,000 mutated
       transactions, and on every Go vector. Speed against Go is not measured
       yet: the host load stayed between 20 and 160.
2. [ ] Benchmark mainnet replay against Go: blocks per second, peak memory,
       disk growth.

Mining, block templates and Stratum come last.

## Rules

- Keep: differential tests against the pinned Go oracle, errors that stop
  instead of guessing, no `unsafe` code, pinned dependencies, the Go-free
  default build.
- A fail-closed limit is only correct when Go has the same limit. A limit that
  Go does not have makes the node stop on a valid block.
- Do not start new performance tuning before the Phase 1 gate, except the
  Phase 4 items above.

## Findings in go-quai v0.57.0

- `p2p/protocol/handler.go:49-92` `ProcRequestRate`: the per-peer request
  rate limit never triggers. `tracker` is a copy of the map value and is never
  written back; the average on line 67 misses the division by 100 for its first
  term; `minPeriod` is computed but not used. Other bounds remain: 100 queued
  messages and 16 MiB per stream, 256 MiB in total.

## Execution log

| Date | Step | Result |
|---|---|---|
| 2026-11-17 | Phase 0 | First commit `59bc106`; `results/` ignored |
| 2026-11-17 | Phase 1.1 | go-quai v0.57.0 mainnet node started for slice `[0 0]` |
| 2026-11-17 | Phase 1.1 | Node guard also stops the node on an iPhone hotspot (`daec049`) |
| 2026-11-17 | Phase 0.4 | Reports moved to `docs/history/`; porting map counts fixed (`3d73836`) |
| 2026-11-17 | Phase 1.2 | QEX1 exporter (`59a928c`); live-clone export of zone 0–11,682: 20,408 records, 75.9 MB; every self-check and `-verify` pass |
| 2026-11-17 | Phase 2.2 groundwork | Rust libp2p talks to go-quai mainnet peers, read-only (`9a25090`): `/quai/1.0.0` requests 57/57 local and 153/153 public, hashes equal to Go RPC; gossip decoded |
| 2026-11-17 | Phase 1.3, 1.7, 2.1 | `quai-node replay` (`0e13a64`): mainnet zone 0–11,682 passes all stateless checks; all 11,682 executed zone blocks have every root equal to go-quai. Rerun independently by the coordinator: PASS, 751 s under load 20 |
| 2026-11-17 | Phase 1.5 | Gas is the only EVM and tx-path bound, as in Go (`130b106`); +19 Go oracle vectors; 246/246 release tests in execution, primitives, consensus |
| 2026-11-17 | Phase 1 | Regression on merged main: mainnet replay 0–11,682 with `--state` still passes, every root equal |
| 2026-11-17 | Phase 3.3, 4.1 | libsecp256k1 sender recovery, decoder fuzz tests, 112 fewer unwraps (`5df7277`) |
