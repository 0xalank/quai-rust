# Offline fee and payout oracle

This adapter follows Dominant Strategies go-quai v0.57.0, revision
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`, under the project's GPL-3.0-only
terms. Preserve the upstream attribution and retained license notices.

`scripts/gen_payout_vectors.py` extracts the official fee/reward branch of
`core/state_processor.go` verbatim at build time into this explicit offline
adapter. The source manifest records that branch hash, the upstream file
hashes and every harness/driver hash. Normal Rust builds never invoke it.

The 486 cases cover selected fork boundaries, three locations, fee feedback,
inclusion depth, algorithm factors, time penalties, reward denomination and
external/duplicate workshares. Work hashes and placement are controlled inputs.
These are arithmetic/assembly projections, not native proof or full block
admission evidence. The separate 16-block corpus runs actual Process with
native proof kernels in an early private profile.
