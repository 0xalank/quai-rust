# quai-rust roadmap

This is the single plan and status file for the port. Update it when work
lands. Do not add a new `*_RESULTS.md` report for each step; put the evidence
in the commit message and keep raw run output in `results/` (local, ignored).

**Measure of progress:** the highest mainnet block that Rust replays with every
root equal to go-quai v0.57.0 (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`).

Current value: **zone 3,860,000** (2026-11-20), from the rolling replay
(`scripts/mainnet/rolling_replay.sh`), past MaxCodeSize (zone 3,490,000). The long replay executes mainnet zone blocks from the
colosseum genesis, and every root equals go-quai: EVM state, ETX set, UTXO,
receipts, outbound ETXs, gas and state counters, account count. It passed
zone 259,200 (the first user transactions), CoinbaseLockupPrecompile at zone
604,800, QiActivation at zone 1,220,000 and ControllerKickInBlock (prime
262,000, at zone 1,223,601), with the first Quai-to-Qi conversions and the
first feedback exchange rate (prime 266,001). Binaries: `f841a74` for
0–180,000, `e1f4cfb` for 180,000–686,009, `d207b70` (the `controllers`
branch, merged as `c466449`) for 686,009–1,250,000, `c466449` for
1,250,000–1,450,000, `6e6ce8f` to 1,618,455, `23d29bd` to 3,300,000 (61–65
blocks/s with 6 PoW threads on a quiet host), then `f5e0c58`. A single-binary
run from genesis with the final binary is still to do.
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
| G5 | Durable state, reorg, crash recovery | Mainnet store, state and checkpoints crash-safe; tip mode: a state version per canonical block, reorgs across commits and a compaction and crashes in a reorg tested on the private fixture |
| G6–G7 | P2P and sync | Mainnet sync by height from public peers; stateless head-follow live with every check and go-quai's head choice |
| G8 | Mainnet shadow sync | Shadow run (test tool) and divergence detector; 2 h 20 min live, 0 divergences; the 2-week run not started |
| G9 | Live RPC, tx pool, wallets | Reads on replayed state; tx pool equal to Go (363/363 oracle steps); filters and WebSocket subscriptions (311/311 oracle steps) and the Qi and conversion methods (385/385 oracle calls) equal to Go; Quais.js alpha.57 and alpha.58 run against the node; a submitted transaction reaches miners only through sharing clients |
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
3. [x] Follow the live head. Send gossiped blocks and workshares through the
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
       Done (branch `head-follow`): a validating stateless run at the live
       head, end to end through the switch from the sync by number to
       gossip, from a shadow bootstrap (step 4, a test tool). In tip mode
       every gossip block passes every stateless check (proof-of-work
       included) and go-quai's head choice; every gossip workshare passes
       go-quai's intake (`worker.AddWorkShare`: `validateUncleData`, then
       `UncleWorkShareClassification` must be Valid) and the uncle rules that
       do not depend on an including block (`validation::verify_workshare`:
       proof ID, difficulty after the parent, prime terminus, SHA and scrypt
       controllers, number), after go-quai's intake distance. Tip state with
       `--state`: every canonical tip block commits its own state version, a
       reorg restores the common ancestor's version (no more "restart from an
       older checkpoint"); the tip base, 240 canonical blocks below the head
       (4 x the 60-block horizon), is final, and checkpoints in tip mode (and
       at the end of a run) are written there, so a restart never resumes on
       a removed branch; a base checkpoint compacts the state and executes
       the canonical blocks above the base again into the compacted file.
       Tests (`tests/tip.rs` 9/9, the fixture's branches): a reorg across the
       per-block commits and a compaction below the common ancestor, a reorg
       to a compacted base (with and without state), a crash after the
       rollback and inside the new branch (resume at the base checkpoint,
       every root of the new branch equal), a block that forks below the base
       refused without stopping. Fixed on the way: SIGTERM in tip mode ended
       the run with "gossip stopped" and no checkpoint; a block forking below
       the base made the follower fetch the same parent forever; the peer
       source never fetched the batch that holds `--to` when `--to` does not
       end a ten-block batch (a bootstrap stalled 8 heights short); a peer
       could become two sources; share views were decoded as full work
       objects and refused.
       Left: a stateful run at the head needs state near the head: peers
       serve no state (go-quai `GetTrieNode` is "todo"), and the rolling
       replay is at zone 1.45M of 11.2M.
4. [ ] Run next to a Go node and compare heads and roots.
       Shadow run, a TEST TOOL and not for production (branch `head-follow`):
       `quai-node run --stateless --shadow-bootstrap <zone block hash|auto>`
       starts at a trusted zone block T instead of the genesis. T and its
       ancestry are NOT validated by the node, which warns so at every start;
       every block after T passes every stateless check, proof-of-work
       included. auto takes the block 1,000 below the best peer head that at
       least 3 peers report as canonical, and `headers/bootstrap.json` keeps
       the peers' heads and answers. The node fetches the prime blocks
       P-8,000..=P-4,002 (P: the prime head at T) for their exchange rates,
       then the zone heights from the prime block P-4,002 to T with their
       prime and region records, every record hash-linked to the one before
       it and the last one T, and derives the window of a replay at T
       (`processor::bootstrap_checkpoint`): append snapshots checked against
       the next headers' commitments and PCRC, rollups against the ETX rollup
       roots and region manifests, the interlinks of P from its proof-of-work
       rank, the token choice sets of P-1 and P rebuilt from the inbound ETXs
       of the last 4,001 prime blocks (a row depends only on its block, its
       parent, its inbound ETXs and the rate 4,000 prime blocks back). On the
       fixture the derived window equals a replay's record by record, and the
       next blocks pass (`tests/shadow.rs`). Peers serve no state (go-quai
       `GetTrieNode` is "todo"), so the flag needs `--stateless`; nothing else
       is skipped after T. The checkpoint format gains an optional trailing
       section with T (needed: the provenance must travel with the
       checkpoint), which every later checkpoint keeps; `run` without the
       flag and every replay refuse such a checkpoint and start only from the
       genesis or a checkpoint the node validated itself
       (`run_refuses_a_shadow_checkpoint_without_the_test_flag`).
       Divergence detector (`divergence.rs`): a block the node rejects that
       the peers' canonical chain holds; a gossip workshare the node rejected
       that a block passing every check includes; the canonical block 5 below
       the head differing from the majority of the answering peers for a
       minute; the peers 10 blocks ahead for 3 minutes. Each one is logged
       with the block, the check, the expected and the actual value, appended
       to `<datadir>/node/divergences.jsonl` and counted in `/metrics`
       (`quai_node_divergences_total{kind}`, with the workshare results, the
       rejected blocks, the unavailable checks and the comparisons).
       `scripts/mainnet/shadow_run.sh` (TEST TOOL): the long stateless run,
       started again after a crash; guards as `rolling_replay.sh`: no run on
       an iPhone hotspot gateway, a disk floor, a STOP file, a lock, a pinned
       binary with its sha256, logs per start and `history.jsonl`.
       Live, `shadow_run.sh` with binary `f65ed71` (sha256 `e45f562e…`),
       2 h 20 min (8,407 s) on public peers, the host at load 170-290. auto
       chose T `0x7f82…6c5c` at zone 11,200,137, 1,000 below the best peer
       head 11,201,137, canonical for 11 of 12 peers (one lagging peer at
       8.68M had none); P at prime 2,478,094; 18,004 heights adopted (4,002
       prime, 9,738 region records, 4,001 token choice rows) in 90 s. Then
       962 heights by number and the switch to gossip at 11,201,099, every
       block with every stateless check, to 11,202,627, the peers' head at
       the end: 2,642 heights, 0 failures, 0 rejected blocks; in tip mode
       1,985 blocks offered, 1,806 accepted, 178 ancestors fetched; 145
       reorgs removing 152 blocks (139 of depth 1, 5 of 2, 1 of 3), none
       refused by the horizon; 15,244 gossip workshares: 14,488 passed every
       check, 92 every check but the parent rules (parent not held), 0
       rejected; the canonical block 5 below the head equal to the peers'
       264 of 264 times; 0 divergences of any kind, 0 unavailable checks. CPU
       590 s (7.1% of one core), RSS median 195 MB, max 345 MB (60 s
       samples); a final checkpoint at the tip base 11,202,387. Earlier runs
       found the bugs fixed above (a stalled bootstrap, share views refused at
       decode). The block store grew about 50 KB per block, so `shadow_run.sh`
       runs `run --no-store` (no RPC) unless SHADOW_STORE=1.
       Next: the 2-week run: `scripts/mainnet/shadow_run.sh` from the merged
       checkout (defaults: SHADOW_BOOTSTRAP=auto, SHADOW_DURATION two weeks,
       metrics on 127.0.0.1:9102, MIN_FREE_GB 30). The stateless run compares
       heads, blocks and workshares, not state roots: a stateful run next to
       a synced Go node needs state near the head (step 3).

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
       `sendRawTransaction` and the pending nonce: done in step 2. A peer
       export has no receipts: receipts are `null` and logs an error.
       Filters, subscriptions and the wallet methods done (branch
       `rpc-complete`: `c44d848`, `e66e47a`, `4353ebb`, `0c5d77a`, `7654d4e`,
       `e865954`, `35e1e37`, `4b9d3b1`).
       go-quai's filter API and event system (`quai_rpc::native::filters`,
       quai/filters/api.go and filter_system.go): `quai_newFilter`,
       `quai_newBlockFilter`, `quai_newPendingTransactionFilter`,
       `quai_getFilterChanges`, `quai_getFilterLogs` and
       `quai_uninstallFilter`, with Go's criteria types (null included),
       errors, IDs, 5-minute deadline and timeout ticker. JSON-RPC over
       WebSocket (`quai_rpc::ws`) on the RPC listen address (an upgrade of
       "/", as go-quai serves WebSocket on a shared HTTP port; loopback by
       default; go-quai's origin rule, 32 MiB messages, a ping after 30 idle
       seconds): `quai_subscribe` "newHeads" (`RPCMarshalWorkObject` v1),
       "logs" and "newPendingTransactions", `*_unsubscribe` of the same
       connection, batches, frames ending in a newline, notifications after
       the subscribe answer, the subscription limit (`--rpc.ws-max-subs`,
       1000). Head events follow go-quai's core: the logs of each block that
       became canonical, oldest first, then one head event; never removed
       logs. The Qi and conversion methods (`native::qi`): `quai_qiToQuai`,
       `quai_quaiToQi`, `quai_calculateConversionAmount`,
       `quai_estimateFeeForQi`, `quai_getOutpointDeltasForAddressesInRange`,
       `quai_getLockedBalance`, `quai_getWrappedQiDeposit`,
       `quai_listRunningChains`, and `net_listening`: 55 native methods.
       Quotes keep `big.Int` signs (negative after KawPow below 3e11
       difficulty); JSON members match without regard to case, as
       encoding/json does (Quais.js sends `previousOutpoint` and `pubkey`).
       New `ChainView` hooks, each with a default: `canonical_hash`,
       `block_or_candidate`, `utxo_set_size`, `spent_utxos`, `locked_quai`,
       `running_chains`; the store view implements all but `locked_quai`,
       whose default is go-quai's answer without the address index (0x0).
       Go oracles (`reference/filters`, `reference/qi-rpc`; the production
       filter API, event system and rpc.Server over HTTP and WebSocket, and
       the production quaiapi handlers), Rust tests through the RPC stack
       (`tests/filters.rs` over HTTP and real WebSocket connections,
       `tests/qi_rpc.rs`): filters and subscriptions 311/311 steps equal
       (filters 185, expiry 29, subscriptions 72, limit 25; three runs),
       Qi and conversion 385/385 calls equal (17 scenarios across KawPow,
       ShaEquivalentDifficulty, ConversionLockChange and the negative
       logarithm). Quais.js against the Rust node: `examples/fixture-rpc.rs`
       serves the oracle's chain with a moving head (b3, b4, b7, a reorg to
       c6, c7, c8, back to b8) and `sdk/fixture-subscriptions.cjs` runs the
       real client (alpha.57 from the local checkout, alpha.58 as Pelagus
       ships it): no error in either; "block" events through "newHeads" 4,
       7, 6, 7, 8, 8 (one per head move, the reorgs included), polled
       "block" events 4-8, log events through the "logs" subscription (6)
       and through `quai_newFilter` polling (4), `getLogs` 5, and the head
       block parses. Live, read-only, against the local go-quai node
       (`examples/live-new-heads.rs`): 10 "newHeads" notifications of its
       live head (zone 167,437-167,446) and the Rust payload of the Go
       export's zone 27,397-27,406 have the same 115 member paths and types
       (values: the oracle). Historical reads
       (`scripts/check_wallet_rpc_live.py`, `quai-node rpc` on the replayed
       store of the Go export, head 27,406, against the node at head
       198,644): 451/451 answers equal: `quai_qiToQuai` 420 (60 random
       blocks), `quai_getOutpointDeltasForAddressesInRange` 20 and its
       errors 4 (all empty: zone 0 has no Qi output before the kick-in),
       `quai_getLockedBalance` 3, identity methods 4. After `35e1e37`, the
       same script on a store of zone 0-8 (node at head 200,616): 77/77
       equal, with the locked Quai of the miners of zone 1-8 (2/2); the
       locked Quai of the 26 Quai miners of the node's blocks
       200,232-200,632: 26/26 equal (0x0 on both). A filter on the
       node's new blocks cannot be compared live: the Rust view is fixed.
       Remaining differences from Go: go-quai drops events when a
       subscriber is busy (non-blocking sends into channels of 0 or 10
       entries) and orders notifications of different subscriptions by
       goroutine scheduling; Rust queues up to 4,096 messages per
       connection and sends logs before the head. Rust counts the
       subscription limit when the call returns (Go in a goroutine). Inside
       a notification, members are in sorted order where Go writes struct
       order (equal as JSON). A WebSocket syntax error answers "parse error"
       (Go: its JSON decoder's text); both then close. `quai_getFilterLogs`
       and `quai_getLogs` return logs in block order (Go: in the order its 8
       goroutines finish). Not served: the subscriptions "newHeadsV2",
       "newChainBlocks(V2)", "newWorkshares(V2)", "accesses",
       "pendingHeader" and "blockTemplateUpdates" (they need appended-block,
       gossip-workshare and unlock events from the processor, or a miner),
       the `eth_` namespace, `quai_getPendingHeader` (a miner),
       `txpool_getRollingFeeInfo` (the processor's per-block fee figures),
       `quai_getProof`, `quai_clientVersion` and `web3_clientVersion`.
       `quai_getLockedBalance` of a Quai address answers 0x0, as go-quai
       does without `--node.index-address-utxos` (its default; the local
       node ran so). Pelagus reads it with each Quai
       balance and shows no balance when it fails. The value of an indexing
       node needs the processor's lock and unlock amounts per block (the
       `locked_quai` hook). `quai_getOutpointDeltasForAddressesInRange`
       needs the block store with UTXO tables; its trimmed outputs come in
       the processor's order (Go: its trim journal's), and spent outputs of
       a block that a reorg removed, or created below the store's base, are
       unavailable. `quai_estimateFeeForQi` needs state (the head's UTXO set
       size).
2. [x] Tx pool and `sendRawTransaction` (`txpool` branch: `1be8b61`,
       `da7fc2a`, `6e32bff`, `65effe2`, `f83d499`, `d6e1b0d`, `0f9ddd5` and
       the commit of this text). `crates/quai-node/src/txpool` ports go-quai
       v0.57.0's pool (core/tx_pool.go, core/tx_list.go) step for step:
       validateTx and add with Go's error texts; pending and queued lists per
       account; the 5% price bump; AccountSlots, GlobalSlots, AccountQueue and
       GlobalQueue (truncatePending, truncateQueue); eviction of the cheapest
       remotes when the pool is full (container/heap's order and tie rule);
       lifetime eviction; the Qi pool (an LRU of 10,024 entries, no in-pool
       double-spend check, as in Go), Qi expiry, the pool limiter; the reset
       on a new head (included Quai transactions leave by nonce, the child
       block's Qi transactions are removed) and, after a reorg of at most 64
       blocks, reinjection of the dropped branch (Qi ones with a cached fee
       without validation); the journal of local transactions.
       ValidateQiTxInputs and ValidateQiTxOutputsAndSignature with Go's texts.
       Decoding follows protobuf-go's wire rules and ProtoDecode's errors; the
       hash covers ProtoEncode; the sender is the one `tx.Hash()` caches;
       `tx.Size()` gives the 128 KB limit and slots. The node runs a pool in
       `rpc` and in `run` with state (reset on every published head; workshare
       transactions from gossip join through go-quai's 2-second remote queue).
       RPC: `quai_sendRawTransaction`, `quai_receiveTxFromPoolSharingClient`,
       the "pending" nonce, pool transactions in `quai_getTransactionByHash`,
       `txpool_content`, `txpool_contentFrom`, `txpool_status`,
       `txpool_inspect` (38 native methods). Sharing clients
       (`--txpool-sharing-clients`, http:// only, default none;
       `--txpool-sync-tx-with-return`, default true as in Go) get go-quai's
       request byte for byte, with its ids, 2-second timeout and error texts.
       Go oracle (`reference/txpool`, `scripts/gen_txpool_vectors.py`,
       `fixtures/protocol/txpool.json`): the production TxPool and quaiapi
       handlers behind rpc.Server over a test chain (state per head, the UTXO
       set in the database, blocks with transactions), runs at explicit steps
       instead of the 1-second ticker, local httptest sharing clients; 26
       scenarios, 363 steps; three runs byte-identical. Rust
       (`crates/quai-node/tests/txpool.rs`): 363/363 steps equal (answers and
       error texts, sizes, gossip errors, sharing requests, and after every
       step the pending and queued lists in order, the Qi LRU order, locals,
       counts, slots, heartbeats and pending nonces). In the node
       (`tests/tip.rs`, `tests/txpool_node.rs`): the pool on the live view
       follows go-quai's reorg of the private fixture (the reset walks back
       through the dropped branch, which the store no longer holds), and
       answers from a mainnet checkpoint's state. Live
       (`scripts/check_txpool_live.py`; `quai-node rpc` on the Go export
       replayed to zone 27,406, loopback only, no sharing clients, keys that
       never held funds; go-quai's pool over the same head fields and
       accounts): 26/26 answers equal: 18 submissions (balance, base fee
       4,180,298,550, fee cap, size, sender scope, signature, decoding, ETX,
       Qi inputs, outputs and data) and 8 reads (pending nonces, a pool
       transaction by hash, `txpool_*`). The head's gas limit is 0, so both
       pools use go-quai's 12,000,000. Remaining differences from Go: where
       Go's result depends on its random map order (equal pending counts in
       truncatePending, equal heartbeats in truncateQueue, equal gas prices in
       the price heaps after a re-heap, the account order of the pool
       limiter's last step), Rust breaks the tie by address or insertion
       order; sharing clients are http:// only (Go also dials ws://), and for
       an answer body that is not JSON only Go's first-character messages are
       reproduced; protobuf-go's errors start "proto:" with a regular or a
       non-breaking space chosen per binary, Rust uses the regular space; the
       journal is length-prefixed protobuf (Go: an RLP stream); the block
       store keeps the canonical chain only, so after a reorg the pool walks
       back through the blocks it remembered: each head it sees with its
       ancestors (1,024 blocks). A dropped block it never read (the store
       dropped it before the pool saw its head) stops the reset as Go's stops
       for an unrooted chain, and the next head resets the state (Go reads
       such blocks from its database); `txpool_getRollingFeeInfo` (the
       processor's fee window) is not served. Left for the Phase 3 gate
       (wallets): a submitted transaction reaches miners only through sharing
       clients (go-quai v0.57.0 has no transaction gossip, and the node
       publishes no workshares). Filters, WebSocket subscriptions and the Qi
       and conversion methods: done in step 1.
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
2. [x] Benchmark mainnet replay against Go (branch `perf`). The Go side is
       an offline oracle, `reference/replay-bench` (`go build -overlay` into
       the pinned go-quai; GOPROXY=off, GOTOOLCHAIN=local): the same QEX1
       blocks through go-quai's own import path from genesis on fresh
       databases (Core.WriteBlock checks, Core.InsertChain with
       VerifyHeader and its full proof-of-work check, the coordinator's head
       update, which runs StateProcessor.Apply; no pending header for
       miners). `scripts/benchmark_replay.py` measures each run from outside
       (wait4 CPU, peak RSS and footprint, instructions, logical bytes
       written from proc_pid_rusage, data directory size) and alternates
       Rust and Go. Every header's proof-of-work is checked on both sides.
       Go export zone 0-3,000 (no user transactions), from genesis, every
       root equal in all runs; Rust `--jobs 4`, Go GOMAXPROCS=4; median
       (range) of 3 runs each, load 41-319:

       | Per 1,000 blocks | go-quai | Rust main `8e69e41` | Rust `68b8d80` | Rust `c4af194` |
       |---|---|---|---|---|
       | CPU seconds | 207 (172-233) | 143 (133-156) | 29.1 (26.0-33.8) | 26.7 (26.5-31.9) |
       | instructions (G) | 408 (386-426) | 153 (151-153) | 171 (171-172) | 109 (109-112) |
       | wall blocks/s | 0.8 (0.7-3.6) | 4.3 (3.7-8.6) | 41.6 (12.2-131) | 18.2 (9.4-20.9) |
       | peak RSS (MiB) | 419 (354-699) | 173 (147-175) | 182 (175-185) | 131 (130-172) |
       | bytes written per height (KiB) | 62.4 (55.6-63.9) | 20.7 (20.4-20.9) | 20.7 (20.5-20.7) | 20.4 (20.3-20.5) |
       | data directory per height (KiB) | 33.1 | 19.0 | 19.0 | 19.0 |

       Go/Rust per round: CPU 1.45x (1.29-1.49) against main, 6.6x
       (6.1-8.0) against `68b8d80`, 7.8x (5.4-8.7) against `c4af194`;
       instructions 2.7x and 3.7x; peak RSS 2.9x and 3.2x; bytes written
       3.0x. The `c4af194` runs did not alternate with Go (they ran during
       Go's second run). Not equal: Go verifies proof-of-work on its one
       import goroutine and Rust on 4 threads, so wall time favours Rust
       beyond the CPU ratio; Go uses LevelDB and commits every block's
       tries, Rust redb with a commit every 1,000 heights; its unsynced
       LevelDB writes are counted as logical bytes (the kernel charges
       almost no physical writes to it); Go logs warnings only and builds
       no pending header, bloom index or coordinator head choice; the
       host ran two long replays, a go-quai node and other agents. A user
       transaction range is not compared: go-quai needs about 0.2 CPU s per
       height with its proof-of-work, about 15 CPU hours to reach zone
       259,200 from genesis, and no option may skip that work. The go-quai
       node on this host syncs 2-4 blocks/s because of the same sequential
       light checks (about 17 per block at zone 163,549, 270-910 ms per
       append in its log), not only because of its peers.
3. [x] Faster proof-of-work checks, same results as Go. Light path: the
       four dataset items of a round run as independent chains, the kernels
       of a job's block header and workshares run four at a time (16
       chains), and the KISS99 program of a period is drawn once per kernel
       instead of once per lane and round. CPU per light verification of the
       1,658 real block and workshare headers of zone 1-200, one thread,
       median of 3 alternating runs under load 60-300
       (`scripts/benchmark_replay.py pow`, CPU at loop markers): main
       `8e69e41` 16.8 ms (16.7-21.2), go-quai 21.5 ms (18.6-22.5),
       interleaved items 6.8 ms (6.1-7.3), batched 3.75 ms (3.69-3.92),
       batched with the program drawn once 2.89 ms (2.72-3.04): 5.8x main,
       7.4x Go. Every run printed the same lines as Go (sha256 `5ad36c12`).
       Go vectors 253/253 (ProgPoW epochs 0 and 1, KAWPOW 0, 1 and 396).
       Rust and Go print identical lines (mix, hash and verdict) for 5,269
       real block and workshare headers of zone 1-300, 100,000-100,060,
       259,200-259,300, 600,000-600,050, 720,000-720,050 and
       1,249,950-1,250,000, each also with the nonce, the seal and the
       claimed mix mutated and in the later ProgPoW epochs 1-3: 35,936
       inputs, 679 valid at the block target, 4,590 workshares, every
       mutation rejected; the batched kernels gave 0 mismatches. The data
       ends at prime 268,000, before KAWPOW AuxPoW (prime 1,171,500), so
       no real KAWPOW header was compared; the KAWPOW kernel is covered by
       the Go vectors. Export replay 0-27,406 from genesis with `c4af194`:
       PASS, every root equal, 757 CPU s (PoW threads 681 s, 2.87 ms per
       hash; main thread 76 s), peak RSS 263 MiB (`68b8d80`, `c4af194`)
4. [ ] Full dataset (`replay --pow-dag light|full|auto`, default light):
       the epoch's dataset built in memory on all cores, read instead of
       computing items, dropped when a newer epoch starts; auto only when
       free memory holds it and 1 GiB more (`68b8d80`). Proven on KAWPOW
       epoch 0, a real 1.0 GiB dataset that fit in the 1.7-2.3 GiB free
       (`pow-full-check`, peak footprint 1.09 GB): built on 8 threads in
       284 s (307 CPU s); full equal to light on 400 inputs, sampled items
       equal; 0.33 ms CPU per hash against 5.67 ms light. Not done: the
       ProgPoW epoch-0 dataset is 4.0 GiB, and free memory stayed at
       1.7-2.4 GiB beside two long replays, a go-quai node and other
       agents (one sample at 4.4 GiB), so neither its real-header check nor
       the replay speed-up in full mode was run; `--pow-dag auto` chose
       light ("needs 4.00 GiB and a 1.00 GiB margin, 2.01 GiB free"). From
       the measured parts, the 4 GiB build costs about 1,200 CPU s and saves
       about 2.5 ms CPU per hash, so it pays off after about 50,000 heights
       (9.6 hashes per height) and saves about 16 CPU hours over the
       epoch-0 range of the mainnet replay.
5. [x] Ordered checks (the replay's main thread, `40ec374`). A
       `/usr/bin/sample` profile on zone 720,000-724,000 (user
       transactions; `scripts/profile_sample.py`): 71% of the busy time in
       block execution; the ETX-set root 31.8% (a lazy queue replayed every
       change since the last commit on every block; now it updates the trie
       of its previous root, as World does), crypto-bigint division 8.6%
       self (bit-serial, about 1,000 rounds for a U1024 divided by a small
       number; now `bigdiv::div`, the same quotient), RLP list flattening
       7.9% self (now whole-item copies). Same range, 2 alternating runs
       each: main-thread CPU 40.3-41.0 s -> 26.4-26.6 s (-35%); the
       checkpoint at 724,000 is byte-identical in all four runs (sha256
       `71ca22db`). Afterwards the main thread waits for proof-of-work 80%
       of the time. Left: state reads and commits (redb, about half of the
       remaining busy time), per-block copies of the ETX-queue and account
       overlays (they grow until the commit every 1,000 heights), the other
       wrapping_div call sites (share_rules, about 3%).

Mining, block templates and Stratum come last.

## Rules

- Keep: differential tests against the pinned Go oracle, errors that stop
  instead of guessing, no `unsafe` code, pinned dependencies, the Go-free
  default build.
- A fail-closed limit is only correct when Go has the same limit. A limit that
  Go does not have makes the node stop on a valid block.
- Performance work may run now; every result must stay identical to Go.
  Every header's proof-of-work is checked; no assume-valid.

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

- The transaction pool (core/tx_pool.go), found by the pool oracle
  (`reference/txpool`):
  - `addQiTxs` (line 1260) has no in-pool double-spend check: two Qi
    transactions that spend one UTXO are both accepted and stay until a
    block includes one or they expire.
  - `SendTxToSharingClients` (line 1606): with sharing clients and
    `--txpool.sync-tx-with-return` (default true) a transaction goes to the
    first client without local validation, the client's answer is the RPC
    answer, and the local pool never sees it.
  - `tx.Hash()` (core/types/transaction.go:725) recovers and caches the
    sender with the transaction's own chain ID, so `addTxs` and
    `validateTx` use that sender; a valid signature for another chain ID
    fails only after every validateTx check ("invalid chain id for
    signer").
  - `poolLimiterGoroutine` (line 2489) caps each queued account at
    `len - AccountQueue` transactions: an account with no more than
    AccountQueue queued transactions loses all of them.
  - `qiTxExpirationGoroutine` (line 2557) looks at the oldest entry
    Len/5 times every 10 minutes: with fewer than 5 Qi transactions none
    expires, and a pass removes at most a fifth of the pool.
  - `addQiTxs` checks outputs against `common.NewChainsAdded(expansion)`
    (line 1271), the chains the latest expansion added: at expansion 0
    the 128 locations {i, 0}, so an output to zone 0 of any region passes
    (ETX eligibility refuses it later) and an output to zone 1 or above is
    "emitted to an inactive chain".
  - `reset` (line 1920) removes the Qi transactions of the new head only
    when its parent is the old head; after a jump of several blocks or a
    reorg, Qi transactions that the new blocks include stay in the pool.
    When the new head is missing from the database, `reset` (line 2013)
    dereferences a nil block inside `runReorg` while holding the pool lock.
  - Pending-transaction filters and subscriptions
    (`quai_newPendingTransactionFilter`, `newPendingTransactions`) never
    receive anything: quai/filters/filter_system.go:111 reads `txsCh`, and
    nothing in v0.57.0 sends to it.
  - protobuf-go (v1.36.6, internal/detrand) starts its errors with
    "proto:" and a regular or a non-breaking space, chosen per binary:
    `quai_sendRawTransaction`'s decode error text differs between go-quai
    builds.

- Filters and subscriptions (quai/filters, rpc/), found by the filter
  oracle (`reference/filters`):
  - Nothing sends `RemovedLogsEvent`: `BodyDb.rmLogsFeed` (core/bodydb.go:33)
    is only subscribed (:263). After a reorg, filters and "logs"
    subscriptions get the new branch's logs and never `"removed": true`.
  - A head move sends each new canonical block's logs (core/headerchain.go:924)
    and one `ChainHeadEvent` (core/slice.go:2073); block filters subscribe to
    head events (quai/filters/api.go:229), so after a jump or a reorg a block
    filter and "newHeads" get only the final head.
  - Events go through non-blocking sends (filter_system.go:421-425, 445,
    465, 510) into unbuffered channels (api.go:228, 715, 770): a burst
    drops events while a subscriber is busy. `handleLogs` logs "Failed to
    deliver logs event" when delivery succeeds and nothing when it drops
    (filter_system.go:422-424).
  - A filter with toBlock "pending" and a from block of "latest" or a number
    is a `MinedAndPendingLogsSubscription` (filter_system.go:265) that no
    handler reads: it never receives logs. A "pending"/"pending" filter gets
    only the miner's pending logs. `quai_getFilterLogs` of both returns the
    head's logs (filter.go:135).
  - A criteria's `blockHash` is ignored by live events: such a filter or
    subscription receives every new log (filter_system.go:219).
  - Any `<namespace>_unsubscribe` cancels a subscription of its connection,
    whatever the namespace (rpc/handler.go:368); `["logs", null]` subscribes
    to all logs (api.go:970).
  - The subscription limit is a plain int incremented in the subscription's
    goroutine after the call returns (api.go:177-198 and the other
    subscription methods): concurrent subscribes can pass it. `Logs`
    creates the RPC subscription before its range check (api.go:714-719).
  - An unpolled filter lives one to two timeouts (5 to 10 minutes): the
    timeout loop removes it at the first tick after its deadline (api.go:85,
    102, 109).

- The Qi and conversion methods (internal/quaiapi/quai_api.go), found by the
  Qi oracle (`reference/qi-rpc`):
  - `quai_quaiToQi` prices the requested header with the current block's
    prime terminus (:1620); `quai_qiToQuai` uses the header's own (:1591).
  - `quai_calculateConversionAmount` reads `BlockByNumber(primeTerminusNumber
    - 4000)` as a zone number (:1751); a missing block is (nil, nil) and
    the rate read panics ("method handler crashed"). It checks the ledger
    bits only, so addresses of other zones convert.
  - `quai_estimateFeeForQi` converts with the head's own difficulty (:1051,
    :1064) where the quotes use the prime terminus' miner difficulty.
  - After KawPow a difficulty below `KQuaiDifficultyDivisor` (3e11) makes
    `CalculateQuaiReward` negative (consensus/misc/rewards.go:218-233), and
    the conversion methods answer negative amounts ("-0x...").
  - `quai_getOutpointDeltasForAddressesInRange` derives "created" outputs
    from the block's transactions, not from the UTXOs execution created
    (:449-600): a coinbase to a Qi address before ControllerKickIn is listed
    although execution creates no output. A missing parent header panics in
    `rawdb.FindCommonAncestor` (core/rawdb/accessors_chain.go:893-898).
  - `quai_getLockedBalance` of a Quai address reads an index that only nodes
    with `--node.index-address-utxos` write (core/state_processor.go:1357);
    the flag is off by default (cmd/utils/flags.go:587-591), and such a
    node answers 0x0 for every address. An unlock larger than the record
    clamps it to 0 (core/rawdb/accessors_chain.go:2150-2160).
  - "pending" makes `quai_getWrappedQiDeposit` read an empty
    `state.StateDB{}` and crash (quai/api_backend.go:185-187).
  - Quais.js sends `previousOutpoint` and `pubkey`; go-quai accepts them
    only because encoding/json matches member names without regard to case
    (core/types/utxo.go:387-390).

## Findings in macOS libmalloc

- macOS 26.1-26.3 (libmalloc 792.41.1, 792.60.6, 792.80.2),
  `src/xzone_malloc/xzone_malloc.c`
  `_xzm_xzone_freelist_chunk_block_is_free_slow`: `free()` of a tiny block
  whose first word equals its free cookie (zone cookie ^ address) confirms
  with a freelist walk, and the walk compares the block's byte offset with
  16-byte granule offsets. A live block at index 2 or later of its chunk
  then counts as free, and `free()` aborts with "pointer being freed was not
  allocated". macOS 26.4 (libmalloc 812.100.31) compares granules. A value
  with padding in its first 8 bytes that is copied into a fresh block can
  carry the `cookie ^ address` that malloc has just computed for that block:
  `mpt::Link` did (fixed with `repr(u64)`). `MallocSecureAllocatorNano=0`
  selects the pre-xzone allocator, which has no such check.
  `scripts/stress_tests.sh --probe` counts such blocks.

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
| 2026-11-18 | Phase 3.2 | Transaction pool (`1be8b61`, `da7fc2a`, `6e32bff`, `65effe2`, `f83d499`, `d6e1b0d`, `0f9ddd5`, and the commit of this row): go-quai v0.57.0's TxPool and Qi pool ported step for step; `quai_sendRawTransaction`, `quai_receiveTxFromPoolSharingClient`, the pending nonce, pool transactions by hash, `txpool_content`/`contentFrom`/`status`/`inspect` (38 native methods); sharing clients; workshare transactions from gossip; journal of local transactions; protobuf-go's wire rules. Go oracle (production TxPool, quaiapi handlers and rpc.Server over a test chain; 26 scenarios, 363 steps; three runs identical): Rust 363/363 equal. In the node: the pool on the live view follows go-quai's reorg of the private fixture through the dropped branch (fails with the ancestor walk removed); answers from a mainnet checkpoint's state. Live, `quai-node rpc` (binary of `f83d499`) on the Go export replayed to zone 27,406 (loopback only, no sharing clients): 26/26 answers equal to go-quai's pool over the same head fields and accounts (18 submissions, 8 reads). Replay of the Go export 0-27,406 with `--state` from genesis in a fresh datadir: PASS, 27,406 zone blocks executed with all state roots equal (2,816.5 s, 9.7 blocks/s under load 150-200; binary of `da7fc2a`; replay.rs, processor.rs, execution, consensus, primitives and store unchanged since). `cargo test --release --locked --workspace`: 73 suites, 435 passed, 0 failed, 0 ignored; fmt clean; clippy on quai-node and quai-rpc: no errors and no warnings in those crates |
| 2026-11-18 | Phase 1, 3.2 | Txpool merged (`232c081`): no-fail-fast rerun 442/443; the failure was a loopback relay test that timed out while the KAWPOW cache built at load 106 (27.4 s alone), now 30 s per copy (`184ba2d`). Two release test runs aborted with a malloc "pointer being freed was not allocated" in `Arc::drop_slow` inside `mpt::Trie::update` / `World::root` (replay and precompiles test binaries; crash reports in `results/agents/heap-crash-reports/`); investigation on branch `heap-crash`. Rolling replay with `6e6ce8f`: stopped by the disk floor (29 GiB) after a clean SIGTERM checkpoint at zone 1,594,938, every root equal; restarted after freeing Go logs (log cap now applied), the debug build cache and the redundant `peer-export-public` |
| 2026-11-19 | Phase 1 | Rolling replay (`6e6ce8f`): stopped by the disk floor again after a clean checkpoint at zone 1,617,447, every root equal; restarted after deleting the coordinator's build cache. Then the replay process aborted with SIGABRT 4 minutes after start, with no crash report and no message (likely the macOS 26.3 libmalloc problem under investigation on `heap-crash`, which hits some processes); restarted from checkpoint 1,617,447 with the same binary until the fix merges |
| 2026-11-18 | Reliability | Heap crash (`heap-crash`: `8e25fc5` fix, `0ad5b33` tools). The "pointer being freed was not allocated" aborts in `World::root` (replay and precompiles tests today; state_db and mpt_drop_stress on Nov 13 and 16) are a libmalloc bug on macOS 26.1-26.3 (Findings in macOS libmalloc), triggered by the 7 padding bytes after `mpt::Link`'s one-byte tag: the branch clone copied malloc's own `cookie ^ address` into them, and in about 2 of 256 processes a live `Box<[Link; 16]>` had its free cookie as its first word. No data race, FFI, stack or hardware fault. Before: 40 aborts (fresh-process trie pass 24/2,434, precompiles 9/3,548, replay test 1/12, 6/1,166 with thread caching off; pre-xzone allocator 0/2,186); all 38 logged pointers are 640-byte branch boxes at chunk index 1-22; 8 SIGABRT dumps and one live probe run show the cookie in the first word; Guard Malloc (both modes) over 387,072 trie updates and over precompiles (secp256k1 FFI): no fault. Fix: `#[repr(u64)]` on `Link`. After, same host and load: probe near hits 0 (before: trie pass 888/24 runs, precompiles 30/30, replay test 1,423/12 + 1 abort, `quai-node-6e6ce8f replay --state` of mainnet 0-8 143-151 per run; fixed `quai-node` 0); trie pass 0/1,800; replay test 200/200 (8 parallel, load 300-450); precompiles 0/620, interleaved with the pre-fix binary under the same load: before 5/1,500 aborts, after 0/1,500. Production hit: the rolling replay's `quai-node-6e6ce8f` (pid 80093, launched 00:46:53Z) aborted at 00:51:06Z in `Workspace::finish` on 0xcb5ecea80, a 640-byte block at index 17, with the same stack; stderr had no text (libmalloc writes it only to the crash report unless `MallocDebugReport=stderr`), and ReportCrash wrote `quai-node-6e6ce8f-2026-11-18-185142.ips` 7 minutes later (a hidden `.ips` until then). Risk: a pre-fix `--state` process aborts at its first such block, early in its run, in about 1% of starts; a process past that point is safe for its life; stateless runs have no exposure; the rolling replay starts one process per pass and stops on a failure; the abort changes no state. New `scripts/stress_tests.sh` (`--probe` fails on any near hit). On the fixed code: quai-primitives lib 8/8; replay test 11/11 in 329 runs; precompiles 8/8 in 2,150 runs; mpt_drop_stress, state_db, checkpoints 7/7, tip 3/3, etx_runtime 2/2, polaris 3/3 with the probe (0 near hits); fmt clean. `cargo test --release --locked --workspace` did not finish: after 1 h 40 min of building at load 100-450 it was stopped to keep the disk above the replays' 30 GiB guard (32 GiB free at that time); it needs a run on merged main. |
| 2026-11-19 | Phase 1 | Heap-crash fix merged (`23d29bd`). The rolling replay stopped at the disk floor once more (clean checkpoint at zone 1,618,455, every root equal) and restarted with `quai-node-23d29bd` (sha256 `94c5fc79…`), `REPLAY_JOBS=6`, a 27 GiB floor (the Go node's guard is 25 GiB) and `MallocDebugReport=stderr`. The `6e6ce8f` genesis run was dropped: the next single-binary run from genesis waits for the final binary after the head-follow and perf merges |
| 2026-11-19 | Phase 1 | Rolling replay (`23d29bd`, no malloc errors): stopped at the 27 GiB disk floor after a clean checkpoint at zone 1,654,965, every root equal; restarted with a 26 GiB floor (the Go node's guard stops the node first at 25 GiB). The agents' build directories (about 6 GiB) return at their merges |
| 2026-11-19 | Phase 1 | Rolling replay: stopped at the 26 GiB floor after a clean checkpoint at zone 1,675,221, every root equal; the Go node's guard stopped the Go node at 25 GiB as designed. Restarted with a 24 GiB floor after deleting the stopped node's logs. Free space also falls with other jobs on this disk (865 of 926 GB used) |
| 2026-11-19 | Phase 4.2-4.5 | Branch `perf` (`cc89a28`, `68b8d80`, `40ec374`, `c4af194`, `17d2a40`, `e010158`; main `03f967c` merged as `cf2e2ee`). Go replay benchmark (`reference/replay-bench`, `scripts/benchmark_replay.py`): go-quai's own import path from genesis, every proof-of-work checked on both sides; Go export zone 0-3,000, 3 alternating runs each: CPU per 1,000 blocks go-quai 207 s, Rust main 143 s (Go/Rust 1.45x, 1.29-1.49), Rust `c4af194` 26.7 s (7.8x, 5.4-8.7); peak RSS 419 against 131-173 MiB; 62 against 20 KiB written per height. Light proof-of-work 16.8 -> 2.89 ms CPU per check (go-quai 21.5 ms): items in lockstep, a job's kernels four at a time, the KISS99 program drawn once; identical to Go on 5,269 real headers and 35,936 inputs with mutations and later epochs, and on the 253 Go vectors. `--pow-dag light|full|auto` (default light): full dataset proven on KAWPOW epoch 0 (1.0 GiB, 0.33 ms per check); the 4.0 GiB ProgPoW epoch 0 was not run, free memory stayed at 1.6-2.4 GiB (max 4.4). Ordered checks: incremental ETX-set root, exact fast division, RLP lists: main-thread CPU -35% on zone 720,000-724,000 with a byte-identical checkpoint. Assume-valid dropped by the user; nothing skips a check. On the merge: `cargo test --release --locked --workspace` 455/455 in 74 suites (the run before the merge aborted in the replay suite with the libmalloc SIGABRT that `23d29bd` fixes), fmt clean, clippy without errors on the changed crates; Go export 0-27,406 with `--state` from genesis in a fresh datadir, `--pow-dag auto` (it chose light: 1.84 GiB free): replay PASS, every root equal, 775 CPU s, peak RSS 205 MiB; zone 720,000-724,000 PASS with the same checkpoint (`71ca22db`). Full mode was not run: it needs 4 GiB beside a 1 GiB margin. During the final tests the rolling replay's disk guard stopped it twice (26 and 25 GiB free, 05:56 and 06:35 UTC), each time after a clean checkpoint, and it restarted; this worktree then held 2.4 GB of build output, deleted after the run |
| 2026-11-19 | Phase 2.3, 2.4 | Head-follow (branch `head-follow`, main merged as `c2c2101`): shadow bootstrap, a TEST TOOL (`f93baf2`): `run --stateless --shadow-bootstrap auto` builds the window at a trusted block 1,000 below the head, agreed by 7-11 of 8-12 peers, from about 18,000 fetched zone heights (4,002 prime and about 9,740 region records, 4,001 token choice rows) in 42-102 s; on the fixture the window equals a replay's; `run` without the flag and every replay refuse its checkpoints (test). Tip state version per canonical block and checkpoints at the final base (`f46f00e`; reorgs across commits and a compaction, crashes in a reorg: tests). Workshare intake check (`b418976`), divergence detector (`3e71945`), `shadow_run.sh` (`5c550b4`), `run --no-store` (`811944c`); fixed from live runs: the peer batch that holds `--to` (`7ec892c`), share views refused at decode (`f65ed71`). Live 2 h 20 min (`f65ed71`): 2,642 heights with every stateless check to the peers' head, 145 reorgs (139/5/1 of depth 1/2/3), 14,488 workshares with every check and 92 without the parent rules, 0 rejected, 264/264 head comparisons equal, 0 divergences, 7.1% of a core, RSS at most 345 MB; 20 min with `--no-store` (`c2c2101`): 1,264 heights, 0 divergences, 47 MB of data. Tests: 443/443 in 73 suites at `811944c`; 455/455 in 75 suites at `c2c2101`; fmt clean; clippy (quai-node) without errors or quai-node warnings; replay 0-27,406 with `--state` from genesis in a fresh datadir PASS with `f65ed71` (10,268 s) and PASS with `c2c2101` (7,592 s), every root equal |
| 2026-11-19 | Phase 1 | Rolling replay with `23d29bd`: 18 passes from zone 1,675,221 to 3,300,000, every root equal, 61–65 blocks/s per pass with 6 PoW threads while the host was quiet. User-approved cleanup: the go-quai node DB and logs, `peer-export-genesis` and the old 720k replay folder deleted (41 GiB free). Perf (`4532ce2`) and head-follow (`f5e0c58`) merged; the rolling replay continues with `quai-node-f5e0c58` (sha256 `38ef65a0…`) from checkpoint 3,300,000 |
| 2026-11-19 | Phase 2.4, 4 | Merged main `f5e0c58` (perf and head-follow): coordinator rerun 467/467 workspace in 75 suites, 45/45 `quai-network --features native-transport`, fmt clean. The 2-week stateless shadow run started at 16:04Z (`scripts/mainnet/shadow_run.sh`, test-only `--shadow-bootstrap auto`, `quai-node-f5e0c58`, no block store, metrics on 127.0.0.1:9102, divergences in `shadow/quai-node/node/divergences.jsonl`), ending about 2026-12-03 |
| 2026-11-19 | Phase 2.3, 2.4 | The first shadow run stopped after 969 heights at zone 11,212,903, 70 below the peers' head: the by-number sync asked for a prime batch past the peers' head ("missing: prime 2480940 blocks") and `shadow_run.sh` took the failure for a divergence. No block failed a consensus check. `shadow_run.sh` now restarts after a "peer data" failure (`c08d4f6`); the run resumed at 16:16Z and follows the head through gossip from zone 11,213,029. Open (node): clip by-number batches at the peers' head, or switch to gossip intake before the last batch |
| 2026-11-19 | Phase 3.1 | Filters, WebSocket subscriptions and the Qi wallet methods (`c44d848`, `e66e47a`, `4353ebb`, `0c5d77a`, `7654d4e`, `e865954`, merges of main `54b5052`, `d86ecf0` and `7d461fc`, `35e1e37`, `4b9d3b1`, and the commit of this row): go-quai's filter API and event system (`quai_newFilter`, `quai_newBlockFilter`, `quai_newPendingTransactionFilter`, `quai_getFilterChanges`, `quai_getFilterLogs`, `quai_uninstallFilter`), JSON-RPC over WebSocket on the RPC listen address with `quai_subscribe` "newHeads", "logs", "newPendingTransactions" and `*_unsubscribe`, and `quai_qiToQuai`, `quai_quaiToQi`, `quai_calculateConversionAmount`, `quai_estimateFeeForQi`, `quai_getOutpointDeltasForAddressesInRange`, `quai_getLockedBalance`, `quai_getWrappedQiDeposit`, `quai_listRunningChains`, `net_listening` (55 native methods). Go oracles `reference/filters` (production filter API, event system, rpc.Server over HTTP and WebSocket; 4 scenarios, 311 steps) and `reference/qi-rpc` (production quaiapi handlers; 17 scenarios, 385 calls), each generated three times byte-identical: Rust 311/311 steps (three runs) and 385/385 calls equal. Quais.js alpha.57 and alpha.58 against the Rust node (`examples/fixture-rpc.rs`, `sdk/fixture-subscriptions.cjs`): no error; newHeads, logs, filter polling, getLogs and the head block work. Live, read-only, local go-quai: newHeads structure 115/115 member paths equal; historical wallet reads 451/451 equal (qiToQuai 420, outpoint deltas 20 and 4 errors, locked balance 3, identity 4). Replay of the Go export 0-27,406 with `--state` from genesis in a fresh datadir, binary of `d86ecf0` (main `2a84775` merged; sha256 `d218285f`): PASS, 27,406 zone blocks executed with all state roots equal (655.3 s, 41.8 blocks/s under load 130-150); before the merges, `--state --store` with this branch's binary (sha256 `2f09acf6`): PASS (11,171.7 s). This branch does not change replay.rs, processor.rs, execution, consensus or store. Tests on `d86ecf0` (main `2a84775` merged; `7d461fc` then merges main `bd689c8`, which changes only ROADMAP.md and scripts/mainnet/shadow_run.sh): `cargo test --release --locked --workspace --no-fail-fast` 77 suites, 477 passed, 0 failed, 0 ignored (2 h 9 min at load 100-170, no libmalloc abort); fmt clean; clippy on quai-rpc and quai-node: no errors and no warnings in those crates (47 in crates that this branch does not change). Two earlier runs on `35e1e37` did not finish: cargo got SIGTERM from outside at the disk floor in one, and I stopped one at 25.7 GiB free (logs in `results/rpc-complete/`). The live checks ran before the Go node was stopped |
| 2026-11-19 | Phase 3.1 | rpc-complete merged (`d59b30c`): 55 native methods; filters and WebSocket subscriptions 311/311 Go oracle steps, Qi and conversion handlers 385/385; Quais.js alpha.57 and alpha.58 (Pelagus) run against the node with no error; `quai_getLockedBalance` answers `0x0` as go-quai without its address index. The tested commit `d86ecf0` has the same Rust code (477/477 in 77 suites). Next agent: `phase1-gaps` (batches past the peers' head, long Qi addresses before Polaris, partial AuxPow decode). The genesis replay is paused at zone 100,000 to give the main replay the CPU |
| 2026-11-19 | Phase 1, 2.4 | The rolling replay (`f5e0c58`) and the shadow run both stopped at their disk floors near 18:10Z: the replay after a clean checkpoint at zone 3,480,000, every root equal; the shadow run after 1,702 heights, level with the peers' head (11,214,438), no failure. `past.redb` holds 7.4 GB of the 7.6 GB replay data at zone 3.48M (about 2.1 KB per height; its 1-year window tops out near 13–14 GB). Both restarted at 19:52Z after the coordinator's build cache was deleted (35 GiB free) |
| 2026-11-19 | Phase 1, 2.4 | Rolling replay (`f5e0c58`) passed MaxCodeSize at zone 3,490,000 (code size 32,768, new opcodes) and stopped at the disk floor after a clean checkpoint at zone 3,560,000, every root equal (25.9 blocks/s). Other sessions on this disk use space fast (one holds 9 GB of temp files), so with the user's approval the replay floor is now 20 GiB and the shadow run's 18 GiB. The paused `f5e0c58` genesis run was dropped: the next single-binary genesis run uses the binary after `phase1-gaps`. Shadow run so far: 2,653 heights in its last segment, level with the peers, 0 divergences of any kind |
