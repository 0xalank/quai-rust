Same text as in [CANONICAL_READS.md](CANONICAL_READS.md); omitted here.

# Local node acceptance and native Rust reads

This milestone adds an isolated go-quai launcher, two pinned Quais.js acceptance jobs, typed block/receipt/log scenarios, and a Rust materialized read store. Rust owns the read indexes, block selection, receipt selection, canonical mapping and atomic reload. The wire fields originate from trusted Go RPC exports; Go remains responsible for consensus validation.

Automatic updates are available through the [live ingestion guide](LIVE_INGESTION.md). It also records the separate mined-chain fixture and its test-only genesis/activation settings.

## Build and verify

Requires Rust, Go, Python 3 and Node/npm, with the adjacent `../go-quai` checkout.

```sh
python3 scripts/build.py
npm ci --prefix sdk --ignore-scripts --no-audit --no-fund
python3 scripts/verify_next.py
```

Use `GO_QUAI_DIR` or the build script's `--go-quai` option for a different checkout. The launcher also accepts `--go-quai`. All integration services listen on loopback and use temporary data directories. The local node runs in solo mode with bootstrapping and telemetry disabled; test processes and temporary node identities are removed on exit.

The latest acceptance pointer is [latest-acceptance.json](../../results/latest-acceptance.json). Each run records the direct-Go/client outputs, candidate outputs, raw RPC trace, pinned package versions, hashes and scenario checks. The original fixture and controlled-state suites remain separate and reproducible.

## Runtime modes

| Mode | Native reads | Other reads | Chain view |
|---|---|---|---|
| `gateway` | None | Forward to Go | Upstream |
| `hybrid` | Known `quai_getBlockByHash` results | Forward to Go | Upstream; cached block-hash content is immutable |
| `snapshot` | Indexed blocks by number/hash and indexed receipts, plus basic discovery | Return an explicit availability error | Frozen at the exported canonical head |

Hybrid startup checks the store's chain ID, genesis hash and location against Go. Live receipt and `latest` reads continue through Go, so the service does not treat an old export as current chain state. Known immutable block reads remain available during an upstream outage; uncovered live reads return upstream errors.

The snapshot mode is a bounded offline read view. It is useful for replay, query experiments and validating native receipt selection. A complete history can return null for a missing receipt. A partial history returns availability errors for receipts it cannot resolve, avoiding false “not found” answers.

## Export and run

For an isolated node, run `python3 scripts/fullnode.py` in one terminal. It prints the zone RPC URL and stays running until interrupted. Set `QUAI_TEST_RPC` in a second terminal to that printed URL, then:

```sh
python3 scripts/export_snapshot.py --url "$QUAI_TEST_RPC" \
  --out run/current.json --max-blocks 128
python3 scripts/run.py --mode hybrid --upstream "$QUAI_TEST_RPC" \
  --snapshot run/current.json
```

Exports are bounded to recent blocks by default. `--start-block` selects an explicit starting height. A partial export includes a parent anchor; capture verifies parent links and that the selected head remains canonical. New blocks may extend the reference chain during capture. A competing replacement of the selected head invalidates the capture.

The export contains full Quai RPC blocks and matching receipts, including Quai, Qi and external transaction fields. The importer validates the SHA-256 of the exact payload bytes, schema, identity, duplicate records, canonical continuity, receipt inclusion/index/type and declared completeness. It preserves unknown JSON fields. SHA-256 detects changed data; it does not authenticate the source or prove consensus commitments.

The exporter writes a temporary file, flushes it and atomically replaces the destination. `--merge previous.json` retains earlier immutable records while adopting the new canonical view. The importer rejects conflicting content for a repeated block or receipt and permits the same transaction to have receipts on different branches.

Send SIGHUP to a running Rust process after replacing its export to load the new view. Parsing happens on a blocking worker, and the new view is published atomically only after validation. Invalid, truncated or wrong-network imports leave the previous view available. `/health` exposes the indexed range, head hash, generation, native hits, upstream calls and rejected reload count.

For a standalone frozen example:

```sh
python3 scripts/run.py --mode snapshot \
  --snapshot fixtures/native/scenario-a.json
```

The checked-in scenario exports exercise transaction formats and API behavior; their blocks are seeded, not mined or consensus validated.

## Acceptance coverage

The real local hierarchy is exercised at genesis and restarted against its on-disk database. Client reads and error behavior are compared directly against Go and through both gateway and hybrid modes. Genesis-only Go simulations can return parent-block errors; the tests preserve those errors rather than converting them to successful preparations.

Funded controlled API scenarios provide successful EVM calls, access-list creation and gas estimation. The typed block cases include Quai, Qi and external transactions, receipts, a failed receipt, outbound ETXs and log filtering. Native snapshot SDK jobs verify block and receipt decoding through the same published packages.

The suite checks branch replacement, orphan receipt removal, retained immutable block reads, network identity rejection, checksum/truncation rejection, source outage, and restart persistence. Existing Go tests separately cover HeaderChain topology, UTXO rollback and Qi edge cases. Full block execution/replay, a mined non-genesis full-node chain, continuous ingestion, WebSockets and the Pelagus extension UI remain separate qualification work.

The SDK environment pins published packages `1.0.0-alpha.57` and `1.0.0-alpha.58` using npm aliases and a lockfile. Those releases report older runtime version constants; reports record both values and identify the package independently. Tests use the standard URL-string constructor with a single-zone HTTP configuration. Running the extension's SDK dependency does not establish complete extension compatibility.

## Performance measurements

```sh
python3 scripts/benchmark_native.py --rounds 3 --seconds 2
```

The benchmark compares direct Go, the forwarding gateway, hybrid native block reads, and the frozen Rust read view. It validates every response with the same Go load generator. Hybrid resource totals include both Go and Rust; the frozen service is measured by itself. Each workload keeps eight requests in flight, warms connections, rotates order and records every round.

Reports include throughput, p95/p99 HTTP completion latency, combined server CPU, client CPU and sampled peak server RSS. CPU measurement also spans generator startup/warmup; RSS sampling can miss peaks. Median per-run percentiles are not pooled percentiles. The shared macOS host and short runs make these initial read-service measurements; production decisions still require representative traffic, ingestion cost and an idle Linux host.

The latest raw result is located through [latest-native.json](../../results/latest-native.json). The new benchmark has its own recorded source contract and does not rewrite the original sealed bandit workload or its historical results.

The optional [canonical receipt/log path](CANONICAL_READS.md) adds per-request freshness barriers, indexed filtering and explicit Go fallback. It defaults to off.
