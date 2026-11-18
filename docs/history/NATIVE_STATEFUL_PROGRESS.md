Current work: [consensus/execution results](NATIVE_CONSENSUS_RESULTS.md). The counts below describe the preceding source increment. Full consensus and canonical synchronization remain unfinished.

Current native runtime: [independent private-node processes](NATIVE_PROCESS_RESULTS.md) qualify early-profile sync/reorg, persistent peer identity, restart/kill recovery, cancellation and clean shutdown. Full global consensus/public sync remain unfinished; the tracked goal is active.



# Native stateful implementation work

The five-step build-out remains active against official v0.57.0
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`, with the local fee overlay inactive.
This increment is installed at `/Users/comp/code/soap/quai-rust`.
The source-bound report and installation archive manifest are linked from
`results/latest-stateful.json`. The private staging copy is retained at
`/private/tmp/quai-rust-stateful-20261110`.

The default suite passed 161 tests. Actual-Go differential evidence now covers
1,233 nested VM cases / 14,796 comparisons, 192 signed Quai settlement and
transaction-finalization cases / 2,493 comparisons, 756 Qi settlement cases /
9,483 comparisons, and three linked early private blocks / 40 checks through
actual StateProcessor.Process. The private transport delivered those blocks,
rejected one forged execution counter and recovered the same execution state
after three durable checkpoint-store restarts. Five substantive journal
recovery checks and three peer-boundary/gossip checks passed. Eight Go/Rust
Noise/Yamux persistent-stream exchanges passed.

These are bounded component and early-private execution checks. Full incoming
ETX/lockup/allocation/reward state, historical block replay, sealed full
consensus admission and canonical private sync remain open. The RPC binary
continues to serve its frozen read-only imported view. No performance result
was measured for this increment. Public transport is blocked before dialing.

`results/stateful-progress.json` records the outstanding work.
`results/stateful-install-baseline.json` guards the installed source before
replacement. Preserve historical reports with their original source pins.
Default build/runtime/distribution must retain no Go dependency; explicit
reference oracles are offline development tooling only.
