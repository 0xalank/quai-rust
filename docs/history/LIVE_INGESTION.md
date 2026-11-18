> Historical Go-backed profile. The default build is now standalone Rust; see [README](../../README.md) and [the native plan](PLAN.md). Run these reference commands only inside the isolated lab created by `scripts/build_reference.py`.

# Live local-chain ingestion

The Rust RPC service can now follow an adjacent Go node automatically. It polls a literal loopback HTTP endpoint, builds a bounded block/receipt view, validates index consistency, saves a durable checkpoint, and publishes the new view atomically. Go remains the consensus authority. By default the Rust hybrid service serves known immutable block-hash reads natively and forwards other reads. The opt-in canonical receipt/log experiment is described separately below.

## Run

Build with `python3 scripts/build.py`, then start the isolated node with `python3 scripts/fullnode.py --consensus blake3`. Set `QUAI_TEST_RPC` to the printed zone URL. In a second terminal:

```sh
python3 scripts/run.py --mode hybrid --upstream "$QUAI_TEST_RPC" \
  --snapshot run/live.json --follow --poll-ms 1000 --window-blocks 128
```

An absent checkpoint bootstraps automatically. An existing checkpoint is checked for corruption and network identity. A valid checkpoint can start during an upstream outage: its immutable block hashes remain readable, while forwarded requests return an availability error until a successful identity-checked poll. The first bootstrap requires a reachable source.

`--follow` is opt-in and requires hybrid mode, a literal `127.0.0.1` or `::1` HTTP upstream, a checkpoint path, a poll interval of 100–30000 ms and a window of 1–1024 blocks. Redirects and ambient proxies are disabled. An OS-backed file lock allows one cooperative writer per checkpoint. Manual SIGHUP reload is ignored while the follower owns the checkpoint.

## Consistency, bounds and recovery

Each poll checks chain ID, zone location and genesis. New blocks are followed backward by parent hash; previously captured immutable records can be reused. The selected head is checked again by number after receipt collection, and identity is checked again before a changed view is accepted. A later extension is allowed. A replacement during collection, missing receipt, bad envelope, inconsistent index or identity change rejects the candidate.

A commit writes a same-directory pending file, syncs its data, renames it over the checkpoint, syncs the directory, and then publishes the validated Rust snapshot. The data and canonical progress cursor are in one checksummed file. Restart uses that file; an abandoned pending file is never treated as committed state. OS locks release on process exit, including SIGKILL. Filesystem and hardware power-loss behavior is outside the local crash test.

The view retains up to one configured canonical window and one window of older/orphan blocks. Candidate block/receipt JSON is capped at 64 MiB; individual responses use the configured response limit, and persisted imports remain capped at 128 MiB. Failed polls back off exponentially up to 30 seconds. Each capture has a 30-second network deadline. Failed writes retain the previous in-memory view; a commit interrupted after rename can be recovered from the new valid file on restart.

A reorg replaces the canonical mapping and receipt selection together. Immutable orphan blocks may stay cached within the retention bound. A reorg deeper than the retained window can rebuild the selected recent window; this is not a full historical index. Receipt misses in an offline partial snapshot remain explicit availability errors rather than false null results.

`/health` includes `ingestion.state`, `network_verified`, the indexed head, last observed upstream head, `lag_blocks`, last successful poll time, error counters, commit count and detected reorg count. Lag is relative to the last successful observation; it is not a promise that the upstream cannot advance afterward. Errors make lag unknown and close the forwarding gate. These ingestion reads are separate from the user-facing `upstream_calls` counter. Retained Go/Rust heads can differ temporarily while a poll is in flight.

## Local mined-chain acceptance

```sh
python3 scripts/verify_live.py
```

The suite first mines standard local Blake3 blocks using the unchanged Go CLI. A separate funded fixture then supports a signed test transfer: its explicit test-only bootstrap changes allocation/genesis identity and sets `TimeToStartTx=0`. The fixture CLI is built through a Go overlay adding `reference/localfixture/main.go`; it does not edit go-quai or bypass block, PoW, transaction or state validation. Its public deterministic test identity is never used with real funds. The test driver submits only to a loopback, zero-peer local chain with the expected genesis.

The funded chain is checkpointed and restarted. Its stopped temporary database is restored to a saved ancestor to create a replacement branch, which Go validates normally. This tests ingestion rollback of real blocks and a mined receipt, not P2P fork choice. Qi/ETX replacement and injected source faults use the separately seeded Go API scenarios. Native snapshot receipt checks remain explicitly frozen; live receipts still forward.

Reports save network profiles, mined blocks and receipt outputs, both branch exports, strict RPC comparisons, injected-source request traces and source/binary hashes through [latest-live.json](../../results/latest-live.json). The prior [SDK and read benchmarks](MILESTONE_RESULTS.md) remain separate evidence from this ingestion suite. This change has no new throughput claim. Sustained ingestion cost, Linux measurements, actual Qi/ETX mining, WebSockets, Pelagus UI and the 24-hour/million-read shadow gate remain future qualification work.

## Verified result on 2026-11-05

The installed project passes 19 lifecycle/fault checks, 12 strict RPC comparisons and 12 SDK comparisons using both pinned versions. The suite mines ten local blocks across the standard and funded profiles and verifies one successful signed Quai transfer, its receipt, recipient balance and sender nonce, then their rollback on the replacement branch. See [the current raw report](../../results/live-20261105T155636313798Z/summary.json).

The existing suites also pass: 120 SDK comparisons, 59 strict native/gateway RPC comparisons, nine earlier scenarios, 704 controlled-state gateway comparisons and 280 fixture checks. Twelve Rust tests, eleven Python tests, formatting and Clippy pass. [Combined validation](../../results/live-validation.json) records the exact report paths and hashes.

The optional [canonical receipt/log path](CANONICAL_READS.md) adds per-request freshness barriers, indexed filtering and explicit Go fallback. It defaults to off.
