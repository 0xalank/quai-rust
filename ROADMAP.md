# quai-rust roadmap

This is the single plan and status file for the port. Update it when work
lands. Do not add a new `*_RESULTS.md` report for each step; put the evidence
in the commit message and keep raw run output in `results/` (local, ignored).

**Measure of progress:** the highest mainnet block that Rust replays with every
root equal to go-quai v0.57.0 (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`).

Current value: **none** (no mainnet block has gone through the Rust code yet).

## Status on 2026-11-17

- 6 crates, 37K lines of Rust source, 18K lines of tests and examples. Go has
  about 120K lines of non-test code.
- `cargo test --release --locked --workspace`: 281/281 tests pass in 55 suites.
- `Verifier::validate_block` returns `Unsupported`
  (`crates/quai-consensus/src/verifier.rs`). The RPC serves a frozen snapshot.
- All sync tests use private chain 1337. An older build synced 259,216 private
  blocks with matching roots; only the last 16 had user transactions.
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
2. [x] Go exporter: for each block, the prime, region and zone work objects
       (protobuf) plus receipts, in order from genesis. The header fields carry
       the expected state, UTXO, ETX and receipt roots.
       `scripts/mainnet/export.sh` (default: stop node, APFS clone, restart,
       export from the clone; `--live` clones without a stop). Risk: an export
       takes about 6.5 KB per zone height, so a full mainnet export does not fit
       next to the node on this disk. Before that point, stream segments into
       replay and delete them, compress them (zstd gives 3.8x; needs QEX2), or
       move the data to another volume.
3. [ ] Rust `replay` command: start at mainnet genesis with the official
       allocations and compare every root. Stop at the first mismatch and print
       it.
4. [ ] Fix the first mismatch, then run again. Milestones are the fork heights
       in go-quai `params/protocol_params.go`: KawPow 1,171,500, Singularity
       1,530,500, ShaEquivalentDifficulty 1,755,000, ConversionStability
       1,872,600, SelfDestructRefund 1,919,500, ConversionLockChange 2,237,000,
       SecurityHardening 2,317,000, Polaris 2,444,574 (prime terminus),
       MaxCodeSize 3,490,000.
5. [ ] Remove the EVM limits that Go does not have, so that only gas limits
       execution: 200,000 steps per frame, 1 MiB memory, 4,096 logs or 4 MiB of
       logs per transaction, 128 KiB call input, 4,096 access-list items. Keep
       anti-DoS limits at the P2P edge.
6. [ ] Replace the per-profile code with one chain spec and one fork schedule.
       Then make `validate_block` do real validation.
7. [ ] Check proof-of-work in a separate parallel pass. Header proofs need no
       state.

## Phase 2: node shell

Gate: the Rust node follows the live mainnet head for 2 weeks with no
divergence from Go.

1. [ ] `quai-node` crate: config file, data directory, logs, metrics, and the
       `replay` command from Phase 1.
2. [ ] Public P2P: bootnodes, Kademlia discovery, GossipSub topics, peer scoring.
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

## Phase 4: speed

Gate: mainnet replay is at least as fast as Go and uses less memory.

1. [ ] libsecp256k1 (the `secp256k1` crate) for ECDSA sender recovery.
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

## Execution log

| Date | Step | Result |
|---|---|---|
| 2026-11-17 | Phase 0 | First commit `59bc106`; `results/` ignored |
| 2026-11-17 | Phase 1.1 | go-quai v0.57.0 mainnet node started for slice `[0 0]` |
| 2026-11-17 | Phase 1.1 | Node guard also stops the node on an iPhone hotspot (`daec049`) |
| 2026-11-17 | Phase 0.4 | Reports moved to `docs/history/`; porting map counts fixed (`3d73836`) |
| 2026-11-17 | Phase 1.2 | QEX1 exporter (`59a928c`); live-clone export of zone 0–11,682: 20,408 records, 75.9 MB; every self-check and `-verify` pass |
