# Native Rust protocol and RPC progress

Validated on 2026-11-05. The default distribution contains two Rust executables
and no Go runtime dependency.

- 4,343 native protocol checks pass: 1,711 header/target checks and 2,632
  transaction checks across 288 codec cases and 40 rejections.
- Native transactions cover Quai/Qi/ETX Protobuf, routed IDs, signing messages,
  ECDSA recovery, Qi MuSig2/BIP340 mathematics and typed-RLP encoding.
- 21 Rust tests, formatting and Clippy across all targets/features pass.
  Seventy frozen-view RPC comparisons, 16 pinned SDK comparisons and eight
  standalone checks pass with zero Go runtime calls.
- A reduced source copy builds with Go/gofmt denied, no Go files and no Go
  checkout. Both protocol corpora pass. The normal dependency graph excludes
  reqwest and C secp256k1/database wrappers.
- The accepted allocation change improves geometric mean in-process handler
  rate by 36.2%. Logs were 4.0% slower; chain ID was roughly flat. Gross
  allocated bytes per receipt fell about 49%.
- The separate HTTP run validates 886,763 responses. Block reads
  improved 6.5%; receipts were flat to 2.9% slower; logs improved 1.5%. It did
  not pass the across-workload HTTP speed gate.

The isolated reference profile separately passes 280 controlled fixture,\nJSON-RPC, timeout/backpressure and gateway checks.\n\nSee [round results](NATIVE_ROUND_RESULTS.md),
[acceptance](results/latest-rust.json),
[Go-denied build](results/rust-no-go-build.json), and
[transaction provenance](fixtures/protocol/transactions-source.json).

The server still serves imported frozen views and reports them unverified.
Typed-RLP decoding, UTXO/state validation, full block commitments, auxiliary
PoW, chain selection, execution and P2P/sync remain open. This is a protocol/RPC
increment, not an independent validating client or an EVM speed result.
