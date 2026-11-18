# quai-rust roadmap

This is the single plan and status file for the port. Update it when work
lands. Do not add a new `*_RESULTS.md` report for each step; put the evidence
in the commit message and keep raw run output in `results/` (local, ignored).

**Measure of progress:** the highest mainnet block that Rust replays with every
root equal to go-quai v0.57.0 (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`).

Current value: **zone 686,009** (2026-11-18), the end of the public peer
export. The run goes on toward zone 1,250,000 on a longer export. The long
replay executes mainnet zone blocks from the colosseum genesis, and every root
equals go-quai: EVM state, ETX set, UTXO, receipts, outbound ETXs, gas and
state counters, account count. It passed zone 259,200, where user
transactions start, so Rust executes real mainnet user transactions with equal
roots. Binaries: `f841a74` for 0–180,000, `e1f4cfb` from checkpoint 180,000.
A single-binary run from genesis is still to do.
Earlier value: zone 27,406, rerun from genesis on the Go export (with receipts)
by the coordinator.

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
       export from the clone; `--live` clones without a stop). An export takes
       about 6.5 KB per zone height (about 65 GB for mainnet), so a full
       export does not fit next to the node on this disk. The rolling replay
       (step 3) deletes the segments behind the replay. Not done, and not
       needed for that: zstd (3.8x; needs QEX2) or another volume.
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
       Peer block source done (`2176432`):
       `crates/quai-network/examples/peer-export.rs` writes QEX1 (without
       receipts) from go-quai peers, read-only. Its zone 0–27,406 records are
       identical to the Go export, receipts aside. Speed: about 1,760 zone
       heights/s from the local node and about 595/s from public peers, against
       3.7/s for the Go node's own sync. A public export to zone 686,009
       (4.1 GB, 1,192,401 records, 6.79M transactions) is in
       `~/quai-data/mainnet-v0.57.0/peer-export-public` and passes `verify`.
       Rolling replay done (`e051bc7`): `scripts/mainnet/rolling_replay.sh`
       follows the head with bounded disk. Each pass prunes, extends the
       peer export from public peers to the latest checkpoint plus
       `ROLLING_AHEAD` (default 100,000), runs `replay --state --resume` to
       the end of the export, prunes again and appends one line to
       `results/rolling-replay/history.jsonl`. At the peers' head it polls
       every 10 minutes. `peer-export prune --below <oldest kept checkpoint>`
       deletes the segment files that no kept checkpoint needs; the manifest
       keeps them with `"pruned": true` (docs/mainnet-export-format.md).
       Both readers skip pruned segments; the exporter takes its chain tips
       from the newest complete segment, which prune keeps. Disk use: the
       replay data directory plus about 3 segments (650 MB each). Guards as
       `go_node.sh`: no download on an iPhone hotspot, stop below 30 GiB
       free, a STOP file (the replay stops at its next checkpoint), stop on
       the first replay failure. End-to-end on clones of the long replay
       (checkpoint 670,000) and `peer-export-1250k`: prune 7.5 GiB to
       4.0 GiB in 0.07 s; replay 680,000 to 710,000 PASS across zone 700,000
       on blocks downloaded after the prune; the rebuilt segment
       700,000–799,999 is byte-identical to the one-pass export; after a
       second prune the replay resumes at 710,000 and passes 715,000, and
       reads only segment 700,000–799,999. Not done: zstd segments (not
       needed for bounded disk).
4. [ ] Fix the first mismatch, then run again. Milestones are the fork heights
       in go-quai `params/protocol_params.go`: KawPow 1,171,500, Singularity
       1,530,500, ShaEquivalentDifficulty 1,755,000, ConversionStability
       1,872,600, SelfDestructRefund 1,919,500, ConversionLockChange 2,237,000,
       SecurityHardening 2,317,000, Polaris 2,444,574 (prime terminus),
       MaxCodeSize 3,490,000.
       Fork audit (2026-11-18, branch `fork-audit`): every v0.57.0 fork gate
       above zone 790,000 / prime 170,000, in replay order. p = prime
       terminus, z = zone; "both" = a Go vector on each side of the gate;
       CB = file of the `controllers` branch. Go files: hc = headerchain.go,
       hcv = headerchain_validation.go, sp = state_processor.go, all in core/.

       | Fork | Go gates | Rust | Vectors | Status |
       |---|---|---|---|---|
       | lock start z 1,036,800 | lock byte, gas and state ramp (hcv:399, :719; block_validator.go:455) | validation.rs:64; rules.rs:234 | header v11, consensus limits (both); uncle lock: before only | equal |
       | ControllerKickIn p 262,000 | prime controllers, token choices, rates (slice.go:323-655, 1724-2020; exchange_controller.go; hc:1950-2006) | confirmation.rs:222 (CB) stops `Unsupported`; the CB components match Go but are not wired | controllers, prime-economics, prime-forward, choice-window | after the controllers merge |
       | ControllerKickIn | Qi coinbase, conversions (sp:566, :839; vm/evm.go:715; instructions.go:1112; hcv:317, :682) | incoming.rs; etx.rs; validation.rs:47 | incoming, stateful, settlement `conversion-*` (new), history v20, header v26 (both); p2p checks (block_validator.go:188, 319, 369) none | equal |
       | QiActivation z 1,220,000 | OneOverKqi (protocol_params.go:437) | economics.rs:29 | header quotes (both) | equal |
       | ConversionSlipChange p 285,000 | cubic discount order (slice.go:456, 524, 1841, 1908) | prime_economics.rs:114 (CB) | prime-economics, prime-forward (both) | equal; after the controllers merge |
       | MaxGrindIncrease z 1,865,000 | 1,000 → 10,000 grind salts (vm/evm.go:667) | calls.rs:307 | stateful `create-grind-*` (new, both) | equal |
       | MaxCodeSize z 3,490,000 | code size 24,576 → 32,768 (vm/evm.go:574); new opcodes (interpreter.go:178) | calls.rs:396; mod.rs:548 | stateful `create-code-*` (new), serial-vm (both) | equal |
       | MaxCodeSize | no reorg past 60 zone blocks (hc:966) | none | none | missing; part of the Phase 2.3 head switch |
       | KQuaiChange p 752,000 | kQuai table, holds, increase / 3 (exchange_controller.go:30-69; rewards.go:199) | controllers.rs:106, 149 (CB) | controllers (both) | equal; after the controllers merge |
       | KawPow p 1,171,500 | share fields, AuxPoW, PoW ids, share targets, KawPoW difficulty, entropy, payout slots, 20,000-block conversion hold (hc, hcv, poem.go, types/wo.go, sp:1168-1241, :2129, rewards.go:219-238, vm/evm.go:725, instructions.go:1118) | zone_header.rs, share_rules.rs, rules.rs, validation.rs, payouts.rs, economics.rs, etx.rs, qi_settlement.rs | work-headers, header, history, consensus, payouts, stateful, qi-settlement, settlement (both) | equal |
       | KawPow grace end p 1,301,100 | ProgPoW still valid at it (hc:727; progpow.go:525); its hash is blake3 of the empty AuxPow (types/wo.go:1451, 1474) | header.rs `block_hash` | work-headers (both, new) | fixed `ce6f8e9` |
       | KawPow | AuxPow without donor header or transaction dropped on decode (types/wo.go:1652) | header.rs:102 keeps a header-only AuxPow, rejects one without header | none | gap: peer data only; the history corpus has header-only AuxPows that Go would drop |
       | InclusionDepthChange p 1,330,000 | depth 3 → 4, target ramp, reward depth, payout discount (hcv:259; hc:359-380, 1277; sp:1119, 1263) | validation.rs:497; share_rules.rs:136; history.rs:307; payouts.rs | history, payouts (both) | equal |
       | BlocksPerYear z 6,307,200 | kQuai authority (state_transition.go:391), lockup multiple (protocol_params.go:389), rate override (slice.go:607, 1999) | settlement.rs:209; economic_state.rs; prime_forward.rs:106 (CB) | authority, incoming `*-zone-*` (new), prime-forward (both) | equal |
       | Singularity p 1,530,500 | uncles 16 → 32, SHA/scrypt shares 8 → 16 (hcv:241, 363-380); forfeiture (state/gen_allocs.go:23) | validation.rs:317, 474, 559; historical.rs:68 | history (before only; share limits none), historical (both) | equal (limits by reading) |
       | QiWrappingChange p 1,570,000 | no local UTXO for wrapped Qi (sp:1677) | qi_settlement.rs:203 | qi-settlement (both) | equal |
       | ShaEquivalentDifficulty p 1,755,000 | SHA difficulty anchor (rewards.go:291), lockup revert (vm/evm.go:267), conversion hold | economics.rs:51; calls.rs:253; etx.rs; qi_settlement.rs | header quotes, stateful lockup, settlement `conversion-*` (new), qi-settlement (both) | equal |
       | ConversionStability p 1,872,600 | share EMA and gain (hc:421, 451) | share_rules.rs:218 | history controllers (both) | equal |
       | SelfDestructRefund p 1,919,500 | refund once, opETX/opConvert overflow checks (instructions.go:875, 919, 966, 1142, 1190; state_transition.go:479) | mod.rs:965; etx.rs | stateful, settlement (both) | equal |
       | ConversionLockChange p 2,237,000 | fee signal, conversion reward, Qi slot, unwrap lock 10, trims, kQuai freeze (hc:1888; rewards.go:35-73; sp:1094-1283; protocol_params.go:379; hcv:1097; exchange_controller.go:17) | payouts.rs; economics.rs; incoming.rs; historical.rs; controllers.rs:157 (CB) | payouts, header, incoming, historical, controllers (both) | equal |
       | SecurityHardening p 2,317,000 | opETX memory: larger range, charged (interpreter.go:77-82) | mod.rs `etx_memory_charged` | stateful `etx-memory-*` (new, both) | fixed `0bd0abe` |
       | SecurityHardening | opETX rules, lockup restore, incoming ETX gas, Qi ETX denomination (instructions.go:904, 984-1029; vm/evm.go:232; state_transition.go:240-360; sp:927) | etx.rs; journal.rs:215; settlement.rs:405; incoming.rs:393 | stateful, incoming (pre-fork ETX gas new) (both) | equal |
       | SecurityHardening | CreateETX intrinsic check (vm/evm.go:752) | etx.rs:94 | none: one zone has no eligible ETX destination | equal by reading |
       | Polaris p 2,444,574 | PoW identity dedupe (hcv:254), suicide transaction (state_transition.go:468), founder forfeiture (gen_allocs.go:27) | validation.rs:500; settlement.rs:255; historical.rs:68 | polaris, settlement, historical (both) | equal |
       | Polaris | Qi output address must be 20 bytes; before it longer ones pass (sp:1999) | qi_settlement.rs:156 stops `Unsupported` | none | gap: needs a variable-length UTXO address (Go keeps all bytes in the UTXO and its hash, crops ETX recipients to the last 20, gas to the first 20) |
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
6. [x] Replace the per-profile code with one chain spec and one fork schedule.
       Then make `validate_block` do real validation (`e1f4cfb`).
       `quai_primitives::spec`: `ChainSpec` (colosseum and the private
       profiles) and `ForkSchedule` (every fork height of v0.57.0), checked
       against the Go oracle (`reference/chain-spec`,
       `scripts/gen_chain_spec_vectors.py`, `fixtures/protocol/chain-spec.json`).
       Fork literals in consensus, execution and the EVM are spec lookups now.
       The confirmation logic is shared (the copy in quai-node is gone); the
       private refusals apply only to the private domains. Removed Rust-only
       caps: ETX and lockup overlay change caps, 8,192-ETX rollup caps, header
       data, uncle, manifest and interlink caps. `Verifier::validate_block`
       validates a block against an explicit parent context (spec, history
       windows, state handle) and `quai-node replay` calls it at every height.
       Open: prime controllers after ControllerKickIn (prime 262,000) stop with
       `Unsupported("prime controllers after ControllerKickInBlock are not
       ported")`; legacy long Qi output addresses before Polaris stop
       explicitly; `trie::MAX_LIST_ITEMS` (65,536 items / 64 MiB), the ETX
       auxiliary entry bounds and the auxiliary.rs field caps are still
       Rust-only; the region inbound path assumes one slice (correct for
       expansion 0).
       Before this step, the replay worked around private-profile entry points: it copied
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
2. [x] Public P2P: bootnodes, Kademlia discovery, GossipSub topics, peer scoring.
       Groundwork (`9a25090`): read-only `quai_network::public` and the
       `mainnet-probe` example work with go-quai v0.57.0 (local) and the v0.57.1
       boot peers. Done (`ab2a7ab`): `public::PublicNode` with
       `Config::read_only` (the probe and `peer-export`) and `Config::public`:
       relay client, DCUtR and AutoNAT; `/quai/kad/1.0.0` in both profiles;
       Go's GossipSub parameters and message ID; go-quai's validators before a
       message is accepted or relayed; a genesis check before a peer takes a
       slot; Go's scores and bans; a request rate limit that works; per-IP
       limits; `publish` off by default. Live on mainnet for 5 minutes: 44
       peers, 2 relay reservations, reached through a relay circuit, 859/859
       gossip messages accepted, 0 rejected. Chosen differences from Go (listed
       in `public/mod.rs`): no relay service, the DHT stays a client until the
       node is public, scores per peer and not per topic. Open: the node
       answers inbound requests with "not found" until it has validated state
       (step 3, Phase 3.1), so two Rust public nodes fail each other's genesis
       check; Kademlia contacts with only circuit addresses are not dialed.
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
       (needs API changes), `evm/*`, `quai-rpc`. Closed: `Lockup::decode`
       accepts a 58-byte record with a zero delegate that `encode` writes as
       38 bytes. Go does the same: its writers add the delegate only when it
       is not zero, and `ReadCoinbaseLockup` reads it only from a 58-byte
       record. Test: `lockup::tests::record_layout_matches_go`.
       `quai-execution` and `quai-rpc` (`0c7e159`, `add3bd3`, `7fdf0bd`):
       non-test `.unwrap()` 127 to 0, `.expect()` 9 to 6, panic macros 5 to
       2 (two `assert!` in a const initializer, checked at build time), index
       expressions 452 to 374; counted by `scripts/count_panics.py`
       (`4357880`). No removed call was reachable from input: each had a
       local invariant and now cannot fail by construction or stops with a
       capability or unavailable error, never a consensus result. The 6
       `expect()` state their invariant (serde_json into a `Vec`, BLAKE2b key
       length, 32-byte halves of a 64-byte buffer). Found and fixed
       (`6d4d695`): a call chain as deep as the gas allows (about 700 frames at
       50,000,000 gas) overflowed a 2 MiB thread stack and aborted the
       process, where go-quai grows goroutine stacks. A transaction now runs
       again on a 128 MiB stack when it passes 64 frames (16 in debug
       builds). Go oracle vector `deep-calls` in
       `fixtures/protocol/settlement.json`; it aborts unmodified `a69aa7a`.
       Left: the proto-field unwraps in `quai-primitives` (Header API) and the
       crates outside this pass.

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
| 2026-11-18 | Phase 1.3, 2.1 | Replay runs long: lazy disk state, checkpoints, `--resume`, `replay_loop.sh` (`4828e14`); agent's loop pass: all roots equal to zone 27,406 |
| 2026-11-18 | Phase 1, 2.2 | `peer-export` (`2176432`): QEX1 from peers, identical to the Go export to zone 27,406; public export to zone 686,009. Long `--state` replay on it started (log: `~/quai-data/mainnet-v0.57.0/logs/replay-public.err`) |
| 2026-11-18 | Phase 1.6 | One ChainSpec/ForkSchedule checked against Go; shared confirmation; `validate_block` validates real blocks and the replay calls it (`e1f4cfb`); agent's run: 343/343 workspace + 21/21, replay 0–27,406 PASS from genesis |
| 2026-11-18 | Phase 1 | Long replay: the `f841a74` binary passed every check to checkpoint 180,000 with no failure; resumed there with `e1f4cfb` (log: `replay-public-e1f4cfb.err`) |
| 2026-11-18 | Phase 1 | Long replay passed zone 259,200 (first user transactions); checkpoint 310,000, every root equal. Peer export extended to zone 1,250,000 (`peer-export-1250k`, 7.6 GB, 6 public peers, 0 faults) |
| 2026-11-18 | Phase 1 | Long replay PASS to zone 686,009, the end of `peer-export-public`, every root equal: 506,009 heights from checkpoint 180,000 in 10,347 s (48.9 blocks/s; 67% of the time waits for proof-of-work), 5,193,990 workshares. Continued on `peer-export-1250k` from checkpoint 700,000 (log: `replay-public-1250k.err`) |
| 2026-11-18 | Phase 3.3 | `Lockup::decode` matches Go; test `record_layout_matches_go` (`7206f8c`). Clippy `approx_constant`: Go's `ln2 = 0.6931` kept on purpose (`e3747da`) |
| 2026-11-18 | Phase 2.2 | Public P2P mode (`ab2a7ab`, merge `24b994a`): relay client, DCUtR, AutoNAT; `/quai/kad/1.0.0` in both profiles; Go GossipSub parameters and message ID; go-quai validators before accept or relay; genesis check per slot, Go scores and bans, a working rate limit, per-IP limit; publish off by default. Live 5 min: 44 peers, 2 relay reservations, reached through a relay circuit, 859/859 gossip accepted, 0 rejected. Coordinator rerun on merged main: 344/344 workspace in 65 suites, 43/43 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 1.2, 1.3 | Rolling replay (`0d9c640`, `5efbbb5`, `e051bc7`): pruned segments in the QEX1 manifest, `peer-export prune` and `status`, `scripts/mainnet/rolling_replay.sh`. On clones of the long replay (checkpoint 670,000): replay 680,000–710,000 PASS across zone 700,000 on blocks downloaded after the prune, 710,000–715,000 PASS after a second prune; 19/19 quai-node and 6/6 peer-export tests. Merge `83e5d64`; coordinator rerun on merged main: 347/347 workspace in 65 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 3.3 | `quai-execution`, `quai-rpc`: non-test unwrap 127 to 0, expect 9 to 6, counted by `scripts/count_panics.py` (`4357880`, `0c7e159`, `add3bd3`, `7fdf0bd`); none was reachable from input. Deep call chains (about 700 frames at 50,000,000 gas) overflowed a 2 MiB stack and aborted the process; they now run again on a 128 MiB stack (`6d4d695`); Go vector `deep-calls` aborts `a69aa7a`. 347/347 workspace tests in 66 suites; replay 0–27,406 with `--state` from genesis PASS (1,398 s) |
| 2026-11-18 | Phase 1.4 | Fork audit (`fork-audit`; table in step 4): every v0.57.0 fork gate above zone 790,000 / prime 170,000. Fixed with Go vectors on both sides: opETX memory gas from SecurityHardening (`0bd0abe`; 7 of 15 new vectors failed before), the hash of a ProgPoW header at prime terminus 1,301,100 (`ce6f8e9`; 48 vectors). New Go vectors where Rust already matched: MaxGrindIncrease and MaxCodeSize (`faf33a9`), conversion transactions across the kick-in and both holds (`6c5db59`), incoming ETX gas before SecurityHardening (`5f80e11`), lockup reward years (`0e1036c`). Open: prime controllers (controllers branch), reorg horizon (Phase 2.3), partial AuxPow decode, long Qi addresses before Polaris. 350/350 workspace tests in 66 suites; replay 0–27,406 with `--state` from genesis PASS (1,410 s) |
