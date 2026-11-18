# Controlled state API compatibility

The compatibility harness runs production go-quai API handlers over committed account-state tries, raw UTXO entries and the Go EVM. It compares a recorded corpus with both the direct Go oracle and the Rust forwarding gateway. Expected balances, storage, contract output, UTXO filtering and access-list contents also have independent assertions.

The current run passes **704 gateway comparisons over 31 methods**, plus 704 reference replay checks and 30 denied-method checks. It detects eight seeded response defects and two stale-balance controls. The original 280 fixture/integration checks, five Rust unit tests, eight comparator test groups and Clippy also pass. These counts describe test executions, not independent production transactions.

## Run

```sh
python3 scripts/build.py
python3 -m unittest discover -s scripts -p 'test_compat_rpc.py'
python3 scripts/verify_chain.py
```

The build uses a temporary Go overlay so the helper can import go-quai's internal API package without adding files to the Go checkout. Keep the adjacent checkout available or pass `--go-quai` to the build script. All test listeners use temporary loopback ports and are stopped when the checks finish.

Routine verification replays [controlled-v1.json](corpus/controlled-v1.json); it does not regenerate expected responses. To review a deliberate reference or harness change, inspect the source change, version the corpus as appropriate, and use `python3 scripts/verify_chain.py --record` to capture a new baseline. Do not use recording to conceal a mismatch. The corpus records oracle/harness source hashes and a digest of the Go source snapshot, and replay refuses a changed contract.

## Covered behavior

- Canonical and alternate snapshots at the same height, with different account balances, nonces, storage and state roots; historical parent-state reads remain separate.
- Zone, region, and state-processing-disabled profiles. The oracle backend deliberately retains seeded state while the processing flag is false, to exercise handler-level checks; a full node backend can impose additional restrictions.
- Contract storage reads, successful and reverted EVM calls, gas estimation and access-list generation. A mutating view call writes only its copied state; subsequent storage reads and repeated calls confirm isolation.
- Spendable and locked Qi balances, current-head-only Qi balance restrictions, UTXO lookup, spent-output filtering, canonical outpoint method spelling, and invalid/overflowing ranges.
- Valid and invalid Qi fee estimation, account nonce and pending nonce, discovery, and selected `eth_*` aliases.
- Parameter failures, missing state, unsupported block tags, conflicting selectors, large/fractional IDs, duplicate IDs, mixed batches and notifications.

The strict comparator retains error wording/data, array order, duplicates, unknown fields and null/empty/missing distinctions. Only object member order and outer JSON-RPC batch-response order are ignored. Empty notification responses have one explicit transport exception: Go HTTP 200 versus Rust HTTP 204. The report lists each occurrence; no other HTTP difference is normalized.

The gateway now permits 46 explicit read/simulation methods. Thirty-one have controlled-state evidence in this harness; other allowlist entries still need cases. The invalid spelling `quai_getOutpointsByAddressAndRange` is rejected, and `quai_getOutPointsByAddressAndRange` is tested successfully.

## Evidence and remaining gates

- [Conformance report](../results/chain-conformance.json): configurations, state profiles, exact hashes, pass/fail counts and documented differences.
- [Method coverage](coverage.json): passing case IDs for each method; proxy and native coverage remain separate.
- [Reference build snapshot](../results/chain-build.json): Go revision, tracked-source hashes, diff hash, helper source and executable hashes.
- [Oracle implementation](../reference/chain/main.go) and [comparison implementation](../scripts/compat_rpc.py).

Headers and state are seeded locally. This harness does not mine or validate blocks, synchronize a chain, run a real transaction pool, or test the node's complete backend/index implementation. Switching the selected snapshot checks stale-response behavior; full reorg ingestion and index rollback remain open. No native Rust chain read is implemented by these changes.

The next gates are a controlled full-node launcher and trace capture, pinned Quais.js/Pelagus acceptance flows, block/receipt/log coverage, and full fork/reorg scenarios. M0 and M1 in [PLAN.md](../PLAN.md) remain open until their complete gates pass. Production performance has not been remeasured by this compatibility change.

## Later milestone

The [native read and SDK milestone](../NATIVE_READS.md) adds a real isolated
full-node launcher, pinned SDK jobs and materialized Rust block/receipt queries.
This document retains the earlier controlled-state suite and its original scope.
