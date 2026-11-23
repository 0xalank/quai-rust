# quai-rust

A native Rust full-node client for the [Quai Network](https://qu.ai/), written for consensus, networking, JSON-RPC, mining, and Stratum compatibility with `go-quai` **v0.57.0+** (`33db82f2891b267ba898ca38bd2dd8bf75cbb072`, including the `a60f36fb1` Quai-time AuxPoW liveness rules and `state_repair` fork).

The default build and runtime have **zero Go toolchain or Go runtime dependency**, **zero `unsafe` code**, and **zero `.unwrap()` calls in non-test production source**. Go is used only as an optional offline compatibility oracle for differential vector generation and benchmarks.

---

## Highlights

- **Consensus & Execution Parity**: Full PoEM fork choice, ProgPoW / KAWPOW, SHA-256d and Scrypt AuxPoW with MuSig2 2-of-3 quorum verification & signing, EVM execution, Qi UTXO ledger, ETX hierarchy rollups, and prime controllers. Verified on mainnet (`colosseum`) replay through **zone 7,580,880+** with matching state, ETX, UTXO, and receipt roots.
- **Performance**: On Genesis mainnet replay benchmarks with full proof-of-work verification (`scripts/benchmark_replay.py`), `quai-rust` runs **~7.8x lower CPU** (`26.7 s` vs `207 s` per 1,000 blocks), **~3x lower peak RSS** (`131–173 MiB` vs `419 MiB`), and **~3x fewer bytes written** than `go-quai`.
- **Networks & Multi-Slice Support**: Supports `colosseum` (mainnet), `garden`, `orchard`, `lighthouse`, and `local` environments, with `--slice <R-Z>` multi-slice selection and `ExpandedNodeSet` hierarchy tracking.
- **Public libp2p Networking**: Kademlia DHT (`/quai/kad/1.0.0`), GossipSub with conservative peer/topic scoring and protocol validators, AutoNAT, DCUtR hole punching, relay client + optional circuit relay server (`--p2p-relay-service`), full `handleRequest` peer serving across prime/region/zone views, and optional block/workshare publishing (`--p2p-publish`).
- **Storage & Light Client Modes**:
  - Default pruned state with crash-safe atomic checkpoints and batched `redb` compaction.
  - `--compact-miner`: low-storage standalone full-block mining preset keeping only the active live EVM/UTXO/ETX state tries (aggressively compacted) and the last 256 canonical blocks (~13 MB), with lock-free `past.redb` lockup/trim reads.
  - `quai-node light`: Helios-style trustless light client — header-only P2P sync (`WorkObjectHeaderView` + PoW/PoEM verification, bounded recent header window) with cryptographic MPT proof verification (`quai_getProof`, `eth_getProof`, `keccak(code)`, verified `quai_call`, and verified receipts/txs) over an untrusted `--upstream-rpc`.
  - `--archive`: per-height state version persistence for historical RPC queries.
  - `--index-address-utxos`: workshare, donor, supply analytics, and locked-Quai indexes.
  - `--prune-blocks <N>`: bounded block history retention.
- **100% `go-quai` JSON-RPC Parity**: Serves all **175 / 175** `go-quai` zone RPC methods and WebSocket subscriptions (plus 21 standard wallet/admin/utility methods) across HTTP, WebSocket, and Unix Domain Socket IPC (`--ipc-path`, mode `0600`), with per-transport namespace allowlists, CORS, and `Host` vhost enforcement. See [docs/rpc-parity.md](docs/rpc-parity.md).
- **Transaction Pool**: Full Quai + Qi transaction pool matching `go-quai` (363/363 differential oracle steps), local transaction journaling, and `http://` + `ws://` txpool sharing clients.
- **Built-In Mining & Stratum v1 Pool Server**:
  - Three mining modes: stateful full-block execution (`--mine` / `--compact-miner`, executing incoming ETXs, lockups, and txpool txs via `zone_execution::build_block`), stateless workshare-only mining (`--mine-workshares`, zero chain state), and zero-storage full-block template relay (`--builder-rpc <URL>`, independently verifying remote templates against locally validated headers).
  - AuxPoW template & coinbase assembly, `getBlockTemplate` / `submitBlock` (SHA-256d, Scrypt, KAWPOW), workshare pool, and live `pendingHeader` / `blockTemplateUpdates` / `newWorkshares` WebSocket streams.
  - Native async Stratum v1 server (`--stratum`) supporting SHA-256d (with BIP 310 ASICBoost version rolling), Scrypt, and KAWPOW, VarDiff, worker password directives (`d=`, `frequency=`, `skip=`, `lock=`, `parentupdate=`), PROXY protocol v1/v2, duplicate share detection, and an HTTP + WebSocket pool dashboard API.

---

## Crate Architecture

| Crate | Responsibility |
|---|---|
| `quai-primitives` | Chain specs (`colosseum`, `garden`, `orchard`, `lighthouse`, `local`), fork schedule, work headers, transactions, Protobuf/RLP codecs, MPT commitments, secp256k1/Schnorr/MuSig2 cryptography |
| `quai-execution` | Intrinsic gas, EVM interpreter & precompiles, Qi UTXO validation & settlement, ETX queue roots, lockup precompile |
| `quai-consensus` | ProgPoW & KAWPOW verification (light batched & optional full DAG), SHA-256d/Scrypt AuxPoW, difficulty/share controllers, PoEM ordering, prime controllers |
| `quai-network` | Wire framing, GossipSub topics & peer/topic scoring, Kademlia DHT, AutoNAT/DCUtR/relay libp2p host, peer request serving |
| `quai-store` | Versioned `redb` state store, batched compaction, journal, log index |
| `quai-rpc` | Native JSON-RPC server over HTTP, WebSocket, and Unix IPC (`quai_*`, `eth_*`, `txpool_*`, `net_*`, `web3_*`, `miner_*`, `admin_*`, `debug_*`), filters, subscriptions, CORS/vhost/namespace controls |
| `quai-node` | Full node binary (`run`, `replay`, `rpc`, `observe`), block store, fork choice, txpool, mining worker, Stratum v1 server & dashboard, Prometheus metrics |

---

## Quick Start

### Build

Requires Rust (stable toolchain; edition 2021).

```sh
cargo build --release --locked --workspace
```

The main node binary is `target/release/quai-node`.

### Run a Syncing Node (Mainnet / Testnet)

```sh
# Sync Colosseum mainnet slice 0-0 with RPC (127.0.0.1:8547) and metrics (127.0.0.1:9101)
./target/release/quai-node run --datadir ~/.quai-node --network colosseum --slice 0-0

# Sync Orchard testnet with archive state and address/supply indexes
./target/release/quai-node run \
  --datadir ~/.quai-orchard \
  --network orchard \
  --archive \
  --index-address-utxos \
  --ipc-path ~/.quai-orchard/quai.ipc
```

Supported `--network` values: `colosseum` (default), `garden`, `orchard`, `lighthouse`, `local`.

### Run a Compact Standalone Mining Node (Live Tries Only + Full Blocks)

Keeps only the active live EVM/UTXO/ETX state tries (no historical tries, compacted frequently) and the last 256 canonical blocks (~13 MB), while executing and mining 100% consensus-valid full blocks and workshares with built-in Stratum v1:

```sh
./target/release/quai-node run \
  --datadir ~/.quai-node \
  --network colosseum \
  --compact-miner \
  --coinbase 0x0000000000000000000000000000000000000001 \
  --miner-extra "quai-rust" \
  --stratum \
  --stratum-sha-addr 0.0.0.0:3333 \
  --stratum-scrypt-addr 0.0.0.0:3334 \
  --stratum-kawpow-addr 0.0.0.0:3335 \
  --stratum-api-addr 0.0.0.0:3336
```

### Run a Trustless Light Client & Zero-Storage Mining Node

```sh
# Helios-style trustless light client (header-only P2P sync + MPT proof-verified RPC)
./target/release/quai-node light \
  --datadir ~/.quai-light \
  --network colosseum \
  --upstream-rpc http://127.0.0.1:8547 \
  --rpc-listen 127.0.0.1:8548

# Zero-state workshare-only Stratum mining node (~5 MB disk, no state trie required)
./target/release/quai-node light \
  --datadir ~/.quai-light \
  --network colosseum \
  --mine-workshares \
  --coinbase 0x0000000000000000000000000000000000000001 \
  --stratum

# Zero-storage full-block + workshare Stratum node via verified remote builder relay
./target/release/quai-node light \
  --datadir ~/.quai-light \
  --network colosseum \
  --builder-rpc http://127.0.0.1:8547 \
  --coinbase 0x0000000000000000000000000000000000000001 \
  --stratum
```

Default Stratum ports when `--stratum` is enabled:
- **SHA-256d (BIP 310 ASICBoost)**: `0.0.0.0:3333`
- **Scrypt**: `0.0.0.0:3334`
- **KAWPOW**: `0.0.0.0:3335`
- **Pool Dashboard HTTP / WebSocket API**: `0.0.0.0:3336` (`/health`, `/api/stats`, `/api/miners`, `/api/blocks`, `/api/payments`, `/api/pool/{stats,blocks,shares,workers}`, `/api/miner/{address}/{stats,workers}`, `/api/node/info`, `WS /api/ws`)

Set any `--stratum-*-addr off` to disable an individual listener.

### RPC Transport Security & IPC

By default, HTTP and WebSocket expose only safe public namespaces (`quai`, `eth`, `net`, `web3`, `txpool` on HTTP; plus `miner` read/sub on WS), while Unix IPC (`--ipc-path`) exposes all namespaces (`admin`, `debug`, `miner`, etc.) on a `0600`-permissioned socket.

```sh
./target/release/quai-node rpc \
  --datadir ~/.quai-node \
  --listen 127.0.0.1:8547 \
  --ipc-path ~/.quai-node/quai.ipc \
  --rpc-http-api quai,eth,net,web3,txpool \
  --rpc-cors-domain http://localhost:3000 \
  --rpc-vhosts localhost,127.0.0.1
```

### Replay a QEX1 Export

```sh
./target/release/quai-node replay \
  --datadir ~/.quai-node \
  --export /path/to/qex1-export \
  --state \
  --store \
  --pow-dag light
```

Use `--pow-dag auto` or `--pow-dag full` to build the in-memory PoW dataset when sufficient RAM is available.

### Configuration File (`--config`)

All `run` options can be supplied via a TOML configuration file (CLI flags override file values):

```toml
datadir = "/var/lib/quai-node"
network = "colosseum"
slice = "0-0"
log_level = "info"

[rpc]
listen = "127.0.0.1:8547"
ipc_path = "/var/lib/quai-node/quai.ipc"
http_api = ["quai", "eth", "net", "web3", "txpool"]
ws_api = ["quai", "eth", "net", "web3", "txpool", "miner"]
cors_domain = ["http://localhost:3000"]
vhosts = ["localhost", "127.0.0.1"]

[metrics]
listen = "127.0.0.1:9101"

[sync]
stateless = false
jobs = 4
margin = 64
max_peers = 8
public = true
archive = false
index_address_utxos = false
checkpoint_every = 10000
commit_every = 1000

[p2p]
relay_service = false
publish = false

[txpool]
enabled = true
sharing_clients = ["http://127.0.0.1:9201"]
sync_tx_with_return = true

[miner]
enabled = false
coinbase = "0x0000000000000000000000000000000000000001"
extra = "quai-rust"
lockup_byte = 0
preference = "0.5"

[stratum]
enabled = false
sha_addr = "0.0.0.0:3333"
scrypt_addr = "0.0.0.0:3334"
kawpow_addr = "0.0.0.0:3335"
api_addr = "0.0.0.0:3336"
name = "quai-rust Stratum"
pool_tag = "quai-rust"
no_vardiff = false
proxy_protocol = false
```

Run `./target/release/quai-node help` for the complete CLI reference.

---

## Testing & Verification

```sh
# Full workspace test suite
cargo test --release --locked --workspace

# Public/private libp2p transport tests
cargo test --locked -p quai-network --features native-transport

# End-to-end light client (MPT proof + fraud rejection), compact stateful block builder,
# stateless workshare miner, builder relay, Stratum v1, dashboard, IPC, and RPC security tests
cargo test --locked -p quai-node --test light_and_mining --test stratum_mining

# Verify 175/175 go-quai RPC parity table consistency
python3 scripts/render_rpc_parity.py --check

# Verify 0 .unwrap() and 0 unsafe in production source
python3 scripts/count_panics.py crates/*/src
! grep -rn "unsafe" crates/*/src/
```

### Quais.js SDK Acceptance

Node/npm are used only as offline test tools against recorded vectors and local `quai-node` instances:

```sh
npm ci --prefix sdk --ignore-scripts --no-audit --no-fund
python3 scripts/verify_rust.py --sdk
```

### Offline Go Reference Oracle (Optional)

Differential vector generation and replay benchmarks against a local `go-quai` checkout are strictly opt-in developer tooling and never invoked by default Cargo builds:

```sh
python3 scripts/gen_protocol_vectors.py --go-quai ../go-quai --check
python3 scripts/benchmark_replay.py --help
```

---

## Documentation

- [ROADMAP.md](ROADMAP.md) — development status, mainnet replay milestones, and execution log
- [docs/rpc-parity.md](docs/rpc-parity.md) — complete 175-method `go-quai` JSON-RPC compatibility matrix
- [NETWORK_READINESS.md](NETWORK_READINESS.md) — release gate definitions
- [PERFORMANCE_LEDGER.md](PERFORMANCE_LEDGER.md) — measured performance benchmarks
- [PORTING_MAP.md](PORTING_MAP.md) — Go-to-Rust module coverage map
- [docs/mainnet-export-format.md](docs/mainnet-export-format.md) — QEX1 export format specification

---

## License & Attribution

`quai-rust` is licensed under **GPL-3.0-only** — see [LICENSE](LICENSE).
Upstream protocol and cryptographic attributions are preserved in [crates/quai-primitives/NOTICE.md](crates/quai-primitives/NOTICE.md) and [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt).
