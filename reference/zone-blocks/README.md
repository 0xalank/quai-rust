# Offline whole-zone execution oracle

This adapter follows Dominant Strategies go-quai v0.57.0, revision
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`, under the project's GPL-3.0-only
terms. Preserve the upstream attribution and retained license notices.

The adapter runs actual `StateProcessor.Process` on 16 linked private blocks,
with signed creation/storage transactions, reward-depth payouts and incoming
coinbases. Its full-size ProgPoW work and mixes are independently recomputed
in Rust. The private nonce search requires positive intrinsic entropy so the
later reward calculation has a nonzero denominator.

The adapter supplies controlled order/header-chain context and inbound lists.
Its memory-database wrapper supplies the configured location required by the
official inbound ETX decoder; the production database already does that.
Empty private allocations follow the frozen private chain specification.
This does not qualify hierarchy placement, inbound eligibility, auxiliary
workshares, every economic fork or canonical network admission/sync.

`scripts/gen_zone_block_vectors.py --go-quai ../go-quai --check` reproduces
the corpus explicitly. Default Cargo builds and the standalone runtime never
invoke Go. The native checker rejects 112 forged execution variants atomically
and restores complete trusted local state images between the accepted blocks.
