The active build is the [standalone Rust profile](standalone.json). Its Go-free frozen-view and protocol scope is separate from the historical gateway evidence below. Go-backed scripts now run from the isolated lab created by `scripts/build_reference.py`. See [the native client plan](../PLAN.md).

For the entire client, use the [source coverage map](../PORTING_MAP.md),
[machine map](go-quai-porting-map.json) and [network gates](../NETWORK_READINESS.md).
The inventory below is selected RPC coverage and historical gateway evidence;
it is not whole-node native compatibility. The active standalone profile serves
eight frozen read methods, without state execution, independent validation or P2P.


# RPC compatibility inventory

The inventory tracks 67 candidate methods from selected Go declarations, the gateway and local SDK mapping. Runtime discovery of the complete full-node RPC surface remains open.

Current gateway: **46 allowlist entries**, **31 methods with controlled-state proxy evidence** in the earlier suite. [Native read coverage](native-coverage.json) now records immutable hybrid reads and frozen block/receipt queries. See [controlled-state testing](CONTROLLED-STATE.md), [coverage](coverage.json) and [the report](../results/chain-conformance.json).

The outpoint method spelling is fixed. Errors are compared strictly in the new harness, including message text and data. Both pinned SDK variants now have acceptance jobs. The [live follower](../LIVE_INGESTION.md) adds mined local blocks, a signed Quai transfer, automatic reorg ingestion and crash recovery. Filters/WebSockets, Pelagus UI, mined Qi/ETX coverage, full runtime inventory and sustained qualification remain open.

An allowlist entry is permission to forward a request. Controlled-state evidence exercises production API handlers with seeded StateDB/UTXO/EVM state; it does not establish full-node backend or consensus compatibility. Case lists in [methods.json](methods.json) point to evidence. Empty lists remain uncovered.

| Method | Target | Current evidence |
|---|---|---|
| `eth_blockNumber` | v0.1 | controlled API proxy |
| `eth_call` | v0.1 | controlled API proxy |
| `eth_chainId` | v0.1 | controlled API proxy |
| `eth_clientVersion` | v0.1 | controlled API proxy |
| `eth_createAccessList` | v0.1 | controlled API proxy |
| `eth_estimateGas` | v0.1 | controlled API proxy |
| `eth_gasPrice` | v0.1 | controlled API proxy |
| `eth_getBalance` | v0.1 | controlled API proxy |
| `eth_getBlockByHash` | v0.1 | allowlisted; uncovered |
| `eth_getBlockByNumber` | v0.1 | allowlisted; uncovered |
| `eth_getCode` | v0.1 | controlled API proxy |
| `eth_getLogs` | v0.1 | allowlisted; uncovered |
| `eth_getProof` | later-profile | missing |
| `eth_getStorageAt` | v0.1 | controlled API proxy |
| `eth_getTransactionByHash` | v0.1 | allowlisted; uncovered |
| `eth_getTransactionCount` | v0.1 | controlled API proxy |
| `eth_getTransactionReceipt` | v0.1 | allowlisted; uncovered |
| `eth_sendRawTransaction` | v0.2 | missing |
| `eth_subscribe` | v0.2 | missing |
| `eth_unsubscribe` | v0.2 | missing |
| `net_listening` | later-profile | missing |
| `net_peerCount` | later-profile | missing |
| `net_version` | v0.1 | controlled API proxy |
| `quai_baseFee` | later-profile | missing |
| `quai_blockNumber` | v0.1 | controlled API proxy |
| `quai_calculateConversionAmount` | v0.1 | allowlisted; uncovered |
| `quai_call` | v0.1 | controlled API proxy |
| `quai_chainId` | v0.1 | controlled API proxy |
| `quai_clientVersion` | v0.1 | controlled API proxy |
| `quai_createAccessList` | v0.1 | controlled API proxy |
| `quai_estimateFeeForQi` | v0.1 | controlled API proxy |
| `quai_estimateGas` | v0.1 | controlled API proxy |
| `quai_gasPrice` | v0.1 | controlled API proxy |
| `quai_getBalance` | v0.1 | controlled API proxy |
| `quai_getBlockByHash` | v0.1 | allowlisted; uncovered |
| `quai_getBlockByNumber` | v0.1 | allowlisted; uncovered |
| `quai_getCode` | v0.1 | controlled API proxy |
| `quai_getFilterChanges` | v0.2 | missing |
| `quai_getFilterLogs` | v0.2 | missing |
| `quai_getLockedBalance` | v0.1 | controlled API proxy |
| `quai_getLogs` | v0.1 | allowlisted; uncovered |
| `quai_getOutPointsByAddressAndRange` | v0.1 | controlled API proxy |
| `quai_getOutpointDeltasForAddressesInRange` | v0.1 | allowlisted; uncovered |
| `quai_getOutpointsByAddress` | v0.1 | controlled API proxy |
| `quai_getOutpointsByAddressAndRange` | v0.1 | invalid spelling removed |
| `quai_getPendingHeader` | later-profile | missing |
| `quai_getProof` | later-profile | missing |
| `quai_getProtocolExpansionNumber` | v0.1 | controlled API proxy |
| `quai_getStorageAt` | v0.1 | controlled API proxy |
| `quai_getTransactionByHash` | v0.1 | allowlisted; uncovered |
| `quai_getTransactionCount` | v0.1 | controlled API proxy |
| `quai_getTransactionReceipt` | v0.1 | allowlisted; uncovered |
| `quai_getUTXO` | v0.1 | controlled API proxy |
| `quai_listRunningChains` | v0.1 | controlled API proxy |
| `quai_newBlockFilter` | v0.2 | missing |
| `quai_newFilter` | v0.2 | missing |
| `quai_newPendingTransactionFilter` | v0.2 | missing |
| `quai_nodeLocation` | v0.1 | controlled API proxy |
| `quai_qiToQuai` | v0.1 | allowlisted; uncovered |
| `quai_quaiToQi` | v0.1 | allowlisted; uncovered |
| `quai_sendRawTransaction` | v0.2 | missing |
| `quai_subscribe` | v0.2 | missing |
| `quai_uninstallFilter` | v0.2 | missing |
| `quai_unsubscribe` | v0.2 | missing |
| `txpool_content` | later-profile | missing |
| `txpool_inspect` | later-profile | missing |
| `web3_clientVersion` | v0.1 | allowlisted; uncovered |

Method casing, parameter and error expectations come from the pinned Go implementation. Static declarations do not prove exposure: module flags, transport and node context still apply. Account management, server-side signing, mining/admin/debug and broad txpool APIs are outside the first public profiles.

The [latest milestone results](../MILESTONE_RESULTS.md) report real-node genesis tests, both SDK pins, native-read coverage and measured resource costs. The earlier per-method proxy evidence above is retained separately from native mode coverage.

The [optional canonical read path](../CANONICAL_READS.md) adds positive native receipts and indexed logs with strict freshness barriers. Its new acceptance and timing baseline are recorded separately; coverage and ordering misses continue through Go.

The [2026-11-09 native foundation](../NATIVE_FOUNDATION_RESULTS.md) adds raw
schemas, bounded body/receipt/trie commitments, gas/Qi-input/live-ETX helpers
and application-wire framing. The live RPC surface remains eight frozen reads;
these new libraries do not close full execution, consensus or sync gates.
