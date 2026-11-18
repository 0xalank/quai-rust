# quai-rust roadmap

This is the single plan and status file for the port. Update it when work
lands. Do not add a new `*_RESULTS.md` report for each step; put the evidence
in the commit message and keep raw run output in `results/` (local, ignored).

**Measure of progress:** the highest mainnet block that Rust replays with every
root equal to go-quai v0.57.0 (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`).

Current value: **zone 1,250,000** (2026-11-18), the end of
`peer-export-1250k`. The long replay executes mainnet zone blocks from the
colosseum genesis, and every root equals go-quai: EVM state, ETX set, UTXO,
receipts, outbound ETXs, gas and state counters, account count. It passed
zone 259,200 (the first user transactions), CoinbaseLockupPrecompile at zone
604,800, QiActivation at zone 1,220,000 and ControllerKickInBlock (prime
262,000, at zone 1,223,601), with the first Quai-to-Qi conversions and the
first feedback exchange rate (prime 266,001). Binaries: `f841a74` for
0–180,000, `e1f4cfb` for 180,000–686,009, `d207b70` (the `controllers`
branch, merged as `c466449`) for 686,009–1,250,000. A single-binary run from
genesis is still to do.
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
| G5 | Durable state, reorg, crash recovery | Mainnet store, state and checkpoints crash-safe; reorg rollback tested on the private fixture |
| G6–G7 | P2P and sync | Mainnet sync by height from public peers; gossip head choice live without validation |
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
       | ControllerKickIn p 262,000 | prime controllers, token choices, rates (slice.go:323-655, 1724-2020; exchange_controller.go; hc:1950-2006) | prime_controller.rs; confirmation.rs; block.rs (`c466449`) | prime-transition (9 linked segments), controllers, prime-economics, prime-forward, choice-window; mainnet replay through zone 1,223,601 | equal (merged) |
       | ControllerKickIn | Qi coinbase, conversions (sp:566, :839; vm/evm.go:715; instructions.go:1112; hcv:317, :682) | incoming.rs; etx.rs; validation.rs:47 | incoming, stateful, settlement `conversion-*` (new), history v20, header v26 (both); p2p checks (block_validator.go:188, 319, 369) none | equal |
       | QiActivation z 1,220,000 | OneOverKqi (protocol_params.go:437) | economics.rs:29 | header quotes (both) | equal |
       | ConversionSlipChange p 285,000 | cubic discount order (slice.go:456, 524, 1841, 1908) | prime_economics.rs:114 | prime-economics, prime-forward, prime-transition (both) | equal (merged) |
       | MaxGrindIncrease z 1,865,000 | 1,000 → 10,000 grind salts (vm/evm.go:667) | calls.rs:307 | stateful `create-grind-*` (new, both) | equal |
       | MaxCodeSize z 3,490,000 | code size 24,576 → 32,768 (vm/evm.go:574); new opcodes (interpreter.go:178) | calls.rs:396; mod.rs:548 | stateful `create-code-*` (new), serial-vm (both) | equal |
       | MaxCodeSize | no reorg past 60 zone blocks (hc:966) | none | none | missing; part of the Phase 2.3 head switch |
       | KQuaiChange p 752,000 | kQuai table, holds, increase / 3 (exchange_controller.go:30-69; rewards.go:199) | controllers.rs:106, 149 | controllers, prime-transition (both) | equal (merged) |
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
       Found by the RPC call work (branch `rpc-calls`, Go probes), fixed
       on branch `exec-findings` with a new Go oracle
       (`reference/precompiles`, `scripts/gen_precompile_vectors.py`,
       `fixtures/protocol/precompiles.json`: 915 signed transactions
       through the actual `applyTransaction`, 668 precompile runs through
       `vm.RunPrecompiledContract`; `tests/precompiles.rs`):
       - Fixed (`db104b7`): a creation, also an ETX creation, did not get
         the precompiles in its access list; go-quai's
         `PrepareAccessList` adds them to every message. Init code that
         touched a precompile failed with "invalid access list" and used
         all its gas. 98 vectors (86 failed before) and 9 new incoming ETX
         rows (all failed before). Every other message path shares the
         two journal constructors.
       - Fixed (`d513c90`): `evm::precompile::translate` mapped only the
         zone prefix or 0x00 to a precompile; go-quai's `EVM.precompile`
         maps any first byte. A transaction to another zone's `0x…0N`
         (N 1 to 9) sent an ETX, and a listed CALLCODE, DELEGATECALL or
         STATICCALL target of that form failed. 531 vectors (179 failed
         before). The 12 documented RPC answers now equal Go.
       - Fixed (`31e4a98`, found on the way): `EVM.Call` and
         `EVM.CallCode` check the value against the balance before the
         lockup branch and the address scope; Rust ran the lockup
         function or the scope check first. 18 vectors (4 failed before).
       - Fixed (`8a83279`, found on the way): a frame that fails with
         `common.ErrExternalAddress` (BALANCE, EXTCODECOPY or EXTCODEHASH
         of another zone's address, CALLCODE, DELEGATECALL or STATICCALL
         to it, the address checks of the lockup functions) makes every
         CALL-family caller fail too, up to the transaction or a CREATE;
         Rust let the caller go on. Errors of the Qi ledger stop at the
         caller (`Fault::QiAddress`); SELFDESTRUCT to another zone is out
         of gas. 268 vectors (57 failed before).
       - Not a difference (`4f9e5f7`): the bn256 G2 subgroup (found by
         reading). go-quai's bn256 (github.com/dominant-strategies/bn256)
         checks the subgroup in `twistPoint.IsOnCurve`, as substrate-bn
         does: 86 pairing runs, 4 twist points outside the subgroup. Only
         the RPC error text differed; it is fixed.
       - Not a difference (`5455800`): the other precompiles on 582
         malformed and edge inputs (padding, truncation, gas, failure).
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
       The prime controllers after ControllerKickIn (prime 262,000) are
       ported (`c466449`): `prime_controller.rs` checks each prime block as
       go-quai `verifyParentExchangeRateAndFlowAmount` (slice.go:1724-2018)
       and prices its newly confirmed ETXs as the Append prime branch
       (slice.go:347-655); checkpoints carry an optional prime section. Where
       go-quai panics (division by zero, zero log, discount above 100,000)
       Rust stops with `Unsupported`. A stateless checkpoint of an older
       binary has no region rollups and cannot resume past the next prime
       block; checkpoints with state resume. Open: legacy long Qi output
       addresses before Polaris stop explicitly; `trie::MAX_LIST_ITEMS` (65,536 items / 64 MiB), the ETX
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

1. [x] `quai-node` crate: config file, data directory, logs, metrics, and the
       `replay` command from Phase 1. Started: the crate, `--datadir`,
       `--log-level` and `replay` (`0e13a64`).
       One chain processor (`5011fca`, `processor.rs`): the per-height
       pipeline of `replay` behind a `BlockSource` trait (the QEX1 export;
       public peers). `replay` keeps its results, logs, report and checkpoint
       format. Go export 0-27,406 with `--state`: PASS from genesis, and PASS
       resumed from checkpoint 20,000 with the same counters.
       Block store (`e642a08`, `blockstore.rs`, redb in the checkpoint
       directory): validated blocks, receipts (Go export or own execution),
       order, total entropy, uncle classes, number-to-hash, transaction
       index, UTXOs created per block and an address index; durable before
       each state commit and checkpoint, rolled back to the checkpoint on a
       resume. `ChainView` over it with one snapshot per RPC call
       (`ChainView::pinned`); `quai-node rpc` serves the store, `--export`
       keeps the export view. Fixtures: zone 0-8 253/253 equal from the
       store, receipts of the node's own execution equal to Go's; Go export
       0-27,406 replayed with `--state --store` by the final binary: PASS,
       every root equal (2,755 s, 9.9 blocks/s); `quai-node rpc` on that store:
       fixture zone-0-27406 617/625 equal, 8 documented differences, 0 failed
       (as the export view); 300 random heights asked from it and the live Go
       node: 1,320/1,320 equal.
       `quai-node run` (`39e4862`): sync by height from public peers
       (`peers.rs`, the logic of `peer-export`), validation and storage
       through the processor, checkpoints, polling at the peers' head less
       `--margin` (64); `--stateless`; a TOML configuration file
       (`--config`); Prometheus metrics (`/metrics`: head, blocks, blocks/s,
       failures, reorgs, peers, the peers' head, faults); SIGINT/SIGTERM stop
       `run` and `replay` after the current height with a checkpoint (exit
       143/130). Live from checkpoint 720,000, stateless, public peers:
       8,099 heights in 1,203 s (6.7 blocks/s, host load 60-100), 4 peers,
       0 failures, 0 peer faults; stop and resume from 728,099 (8.4
       blocks/s); with the fetch limit 150 heights above the node it reached
       it and followed the peers' head (+87 blocks in 7.5 minutes, node +90).
       Speed is proof-of-work bound, as in replay.
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
       Done (`43e035d`): go-quai's head choice for slice [0 0]
       (`forkchoice.rs`: NodeSet, BuildPendingHeaders with its 21 tries and
       2,000-entry cache, the best-entropy head, SetCurrentHeader's 60-block
       horizon above zone 3,490,000 and Append's 60-block limit); equal to
       go-quai's coordinator on the private fixture (24/24 heads and scores,
       reference/canonical). Tip mode of the processor (`Processor::offer`):
       a block is checked against its parent's context, the head choice
       picks the head, and a reorg rolls back the executed state, the block
       store and its indexes to the common ancestor and executes the new
       branch; undo data for 240 blocks (4x the horizon). Test: the
       fixture's two branches with and without state; the head moves at the
       same block as in Go, 8 blocks are removed, every root of the new
       branch equals the header, the store and transaction index follow, and
       a resume starts at the new head. Head events and removed logs for the
       RPC (`ChainView::head_events`, `removed_logs`). Gossip intake
       (`tip.rs`, `Config::public`, publish off): zone blocks and workshares,
       dominant views and missing ancestors fetched by hash; `run` switches
       to it at the peers' head less the margin.
       Live, `quai-node observe` (gossip, proof-of-work and go-quai's entropy
       and head choice, no validation context): 31 minutes on mainnet gossip:
       433 zone blocks and 4,032 workshares that go-quai's validators accept,
       none failed proof-of-work or entropy; 47 head changes removed blocks (45
       of depth 1, one of 2, one of 3; 50 blocks) between zone 11,197,132 and
       11,197,466; the block 3 below the chosen head equal to the peers'
       canonical hash 28/28 times.
       Left: a validating run at the live head. The node needs a checkpoint
       near it: a stateless sync from zone 730,369 to 11.2M takes about 15-20
       days at this host's 6-8 blocks/s (about 3 days at 35-50 blocks/s on an
       idle host). With state, the tip keeps its state overlay until the next
       checkpoint, and a reorg below the last state commit stops the node
       (restart from an older checkpoint).
4. [ ] Run next to a Go node and compare heads and roots.

## Phase 3: product

Gate: Quais.js and Pelagus work against the Rust node.

1. [ ] RPC on validated state: balances, UTXOs, `call`, `estimateGas`,
       receipts, logs, filters, WebSocket.
       Done (`b69422e`, `4b2cb40`, `1c6ef5a`): `quai_rpc::view::ChainView`
       (head, blocks by number and hash, uncle classes, total entropy,
       transaction positions, stored receipts, account state at a block,
       UTXOs by outpoint, address and range, read-only calls; hooks for head
       events and a transaction pool) and `--mode native` with go-quai's JSON
       for 30 `quai_` read methods, argument errors included.
       `quai-node rpc --export DIR [--listen ADDR]` serves a QEX1 export and
       the latest state checkpoint of `--datadir`
       (`crates/quai-node/src/rpc_view.rs`, `rpc_call.rs`): it rechecks
       every zone block hash and parent link to the checkpoint block and the
       checkpoint roots against the head header; order from the export's
       records, total entropy from the child (natively for the head), uncle
       classes from their proof-of-work. "latest" is the checkpoint; state at
       other blocks only where the head's tries serve it, else go-quai's
       pruned-node answer (`missing trie node <root> (path )`). Go golden
       fixtures (`scripts/gen_native_rpc_vectors.py`,
       `fixtures/native-rpc/`) and an offline test on zone 0-8
       (`crates/quai-node/tests/native_rpc.rs`, 253/253 as recorded). Live,
       Go export and checkpoint 27,406: 617/625 fixture answers equal to Go,
       8 documented differences, 0 failed; 1,000 random heights 4,770/4,770
       equal (`scripts/compare_native_rpc.py`).
       Calls done (`9e6f953`, `a1c55f3`, `4d918e1`, `b6a8e10`, `8d9bba4`):
       go-quai runs `quai_call`, `quai_estimateGas` and
       `quai_createAccessList` with `vm.Config{Debug: true}`, which turns
       access-list checks off.
       `quai_execution::evm::message` runs a message that way, with Go's
       access-list tracer and Go's error texts (stack and opcode operands,
       address scope, CreateETX, grinding, precompiles); consensus
       execution never enables it. `quai_estimateGas` first builds the
       access list of a contract call, as Go does. New methods (32 now):
       `quai_createAccessList`, and `quai_gasPrice`
       (`CalcBaseFee(head) * 120 / 100`; `ChainView::calc_base_fee` has a
       default over consensus code). Go oracle
       (`scripts/gen_rpc_call_vectors.py`: production quaiapi handlers and
       JSON-RPC server; `fixtures/protocol/rpc-calls.json`,
       `crates/quai-node/tests/rpc_calls.rs`): 392/404 equal, 12
       documented; 404/404 after step 1.4 (another zone's precompile, on
       `exec-findings`), when `tests/rpc_calls.rs` drops its check that
       the 12 still differ (`results/exec-findings/quai-node-rpc-calls.patch`).
       Live, checkpoint 27,406
       (`fixtures/native-rpc/calls-0-27406.json`: transfers, new
       accounts, the nine precompiles, access lists, gas edges): 111/111
       equal; the base fee of all 27,406 real blocks equals the next
       header's (`examples/check-base-fee.rs`).
       Remaining differences: lockup-contract errors ("lockup error" where
       Go names the reason); the pointers that Go prints in "failed to
       apply transaction" (Rust prints 0xc000000000).
       Left: filters (`newFilter`, `getFilterChanges` and the others) and
       WebSocket subscriptions need the live head (Phase 2.3);
       `sendRawTransaction` and the pending nonce need step 2. Not served:
       Qi and conversion estimates, `quai_getLockedBalance`,
       `quai_getProof`, the `eth_` namespace. A peer export has no
       receipts: receipts are `null` and logs an error.
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

- `core/vm/evm.go:55-68` `EVM.precompile`: when an address is not a
  precompile, Go puts the zone prefix in its first byte and looks again. A
  call to another zone's `0x…0N` (N 1 to 9, the other bytes zero) therefore
  runs this zone's precompile N and sends no ETX.
- `core/vm/instructions.go:722, 762, 794, 826`: opCall, opCallCode,
  opDelegateCall and opStaticCall end their own frame when the callee
  fails with `common.ErrExternalAddress`, so that error climbs to the
  transaction or a CREATE. `InternalAndQuaiAddress` checks the ledger
  first, so a Qi address gives another error, which does not climb.
- `core/vm/evm.go:260-272`: `EVM.Call` checks the value before the lockup
  branch, so a CALL with value to the lockup contract fails when the
  caller cannot pay it, although the lockup contract never takes the
  value.
- `p2p/protocol/handler.go:49-92` `ProcRequestRate`: the per-peer request
  rate limit never triggers. `tracker` is a copy of the map value and is never
  written back; the average on line 67 misses the division by 100 for its first
  term; `minPeriod` is computed but not used. Other bounds remain: 100 queued
  messages and 16 MiB per stream, 256 MiB in total.
- `core/types/wo.go:750-754` `IsTransitionProgPowBlock` ends the transition
  with `<` KawPowForkBlock + KawPowTransitionPeriod (prime terminus
  1,301,100), but the ProgPoW checks (`core/headerchain.go:727`,
  `consensus/progpow/progpow.go:525`) refuse only `>`. A ProgPoW header at
  exactly 1,301,100 is therefore valid, and `WorkObjectHeader.Hash` names it
  `WoCustomPowHash` of a nil AuxPow: blake3 of no bytes. Every such header
  has the same hash. Rust copies this (`ce6f8e9`) so that hashes stay equal.

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
| 2026-11-18 | Phase 3.3 | `quai-execution`, `quai-rpc`: non-test unwrap 127 to 0, expect 9 to 6, counted by `scripts/count_panics.py` (`4357880`, `0c7e159`, `add3bd3`, `7fdf0bd`); none was reachable from input. Deep call chains (about 700 frames at 50,000,000 gas) overflowed a 2 MiB stack and aborted the process; they now run again on a 128 MiB stack (`6d4d695`); Go vector `deep-calls` aborts `a69aa7a`. 347/347 workspace tests in 66 suites; replay 0–27,406 with `--state` from genesis PASS (1,398 s). Merge `f8a18b9`; coordinator rerun on merged main: 350/350 workspace in 66 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 1.4 | Fork audit (`fork-audit`; table in step 4): every v0.57.0 fork gate above zone 790,000 / prime 170,000. Fixed with Go vectors on both sides: opETX memory gas from SecurityHardening (`0bd0abe`; 7 of 15 new vectors failed before), the hash of a ProgPoW header at prime terminus 1,301,100 (`ce6f8e9`; 48 vectors). New Go vectors where Rust already matched: MaxGrindIncrease and MaxCodeSize (`faf33a9`), conversion transactions across the kick-in and both holds (`6c5db59`), incoming ETX gas before SecurityHardening (`5f80e11`), lockup reward years (`0e1036c`). Open: prime controllers (controllers branch), reorg horizon (Phase 2.3), partial AuxPow decode, long Qi addresses before Polaris. 350/350 workspace tests in 66 suites; replay 0–27,406 with `--state` from genesis PASS (1,410 s). Merge `6b27112`; coordinator rerun on merged main: 350/350 workspace in 66 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 3.1 | Native RPC on replayed state (`b69422e`, `4b2cb40`, `1c6ef5a`, and the commit of this row): `ChainView`, `--mode native` (30 read methods, go-quai JSON and errors), `quai-node rpc`. Replay of the Go export 0-27,406 with `--state` PASS (1,358 s) as the view's state. Live against go-quai v0.57.0: fixture 617/625 equal, 8 documented differences, 0 failed; the 600 equal vectors asked again live 600/600; 1,000 random heights 4,770/4,770. Offline zone 0-8 test 253/253. 358/358 workspace tests in 67 suites (23.6 min), fmt clean, clippy without errors. Merge `417f48e`; coordinator rerun on merged main: 358/358 workspace in 67 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 1.4, 1.6 | Prime controllers after ControllerKickInBlock ported (`4bf54f9`); Go oracle prime-transition (`bb66950`): 73 linked blocks, 584 + 124 Go-rejected changes, 146 Go-accepted, all equal. Mainnet replay of the branch binary `d207b70` from checkpoint 686,009: PASS to zone 1,250,000 (end of `peer-export-1250k`; kick-in at zone 1,223,601, first conversions at 1,223,711, first feedback rate at prime 266,001), every root equal, 38.8 blocks/s. Merge `c466449`; coordinator rerun on merged main: 361/361 workspace in 68 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 1 | Two runs with the merged binary `c466449` (sha256 `db24ef65…`): `scripts/mainnet/rolling_replay.sh` continues from checkpoint 1,250,000 toward the head (`replay-public`, log `logs/rolling.log`); a single-binary replay from genesis on a clone of `peer-export-1250k` (`replay-genesis-c466449`) checks today's execution changes on zone 0–1,250,000 |
| 2026-11-18 | Phase 3.1 | Calls in go-quai's debug mode (`9e6f953`, `a1c55f3`, `4d918e1`, `b6a8e10`, `8d9bba4`, and the commit of this row): no access-list checks, Go's access-list tracer and error texts; `quai_estimateGas` builds the access list of contract calls; new `quai_createAccessList` and `quai_gasPrice`. Go oracle (production quaiapi handlers): 392/404 equal, 12 documented (another zone's precompile). Live, checkpoint 27,406: call fixture 111/111, existing fixture 617/625 with 8 documented (as before), 300 random heights 1,320/1,320; `calc_base_fee` of 27,406 real blocks equal to the next header's base fee. Replay 0-27,406 with `--state` from genesis PASS (4,722 s). 367/367 tests in 69 suites (`cargo test --release --locked --workspace` 366/366, 56 min; the new offline gas-price test 1/1 in a release rerun of its suite), fmt clean, clippy without errors. Found two consensus differences (step 1.4 note). Merge `355c004`; coordinator rerun on merged main: 367/367 workspace in 69 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 2.1, 2.3 | Node shell (`5011fca`, `e642a08`, `39e4862`, `43e035d`): one chain processor with a block source trait; block store with RPC view (zone 0-8 fixture 253/253 from the store; Go export 0-27,406 with `--state --store` PASS, its store's RPC 617/625 equal + 8 documented, 1,320/1,320 live sample); `quai-node run` from public peers (stateless from checkpoint 720,000: 6.7-8.4 blocks/s, 0 failures, stop and resume, following the peers' head); config file, metrics, signals; go-quai head choice (fixture 24/24) and reorgs with state rollback (fixture branches); live gossip observer (31 min: 433 blocks, 4,032 workshares, 47 reorgs of depth 1-3, 3-deep agreement with peers 28/28). Go export replay 0-27,406 with `--state` PASS from genesis (4,123 s) and resumed from checkpoint 20,000 (1,141 s). 417/417 workspace tests in 70 suites, 45/45 `quai-network --features native-transport`, fmt clean, clippy without errors. Merge `cbaf870`; coordinator rerun on merged main: 423/423 workspace in 71 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 1.4 | Execution findings of the RPC call work (`db104b7`, `d513c90`, `31e4a98`, `8a83279`, `4f9e5f7`, `5455800`): a creation (also an ETX creation) gets the precompiles in its access list; another zone's `0x…0N` is the zone's precompile; CALL and CALLCODE check the value before the lockup branch and the address scope; `ErrExternalAddress` climbs CALL-family frames as in Go; bn256 G2 subgroup and the other precompiles: no difference (RPC text of a G2 point outside the subgroup fixed). New Go oracle `reference/precompiles`: 915 transactions through `applyTransaction` (failed on the parent commits: 86/98, 179/531, 4/18, 57/268) and 668 precompile runs; incoming oracle +9 ETX creations (9/9 failed before). 375/375 tests in 70 suites (44.6 min), with the 12-line `quai-node/tests/rpc_calls.rs` change from `results/exec-findings/quai-node-rpc-calls.patch` applied for the run (rpc-calls 404/404); fmt clean; clippy (quai-execution) without errors. Replay 0-27,406 with `--state` from genesis in a fresh datadir PASS (2,770 s, binary `5455800`). Merge `6e6ce8f`; coordinator rerun on merged main: 431/431 workspace in 72 suites, 45/45 `quai-network --features native-transport`, fmt clean |
| 2026-11-18 | Phase 1 | Rolling replay with `c466449`: PASS to zone 1,450,000, every root equal (passes of 100,000 heights, 11–18 blocks/s under load 100–150). Both runs switch to `6e6ce8f` (sha256 `db7810cb…`), which has the node-shell processor and the execution fixes: the rolling replay resumes from its next checkpoint, and a new single-binary run from genesis (`replay-genesis-6e6ce8f`) replaces the `c466449` one, stopped at zone 124,000 |
