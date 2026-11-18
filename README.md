# quai-rust

A native Rust Quai client project. The default build and distribution use Rust
binaries with no Go toolchain, Go checkout, Go node, or upstream gateway.
Go remains an explicit offline compatibility oracle in a separate test profile.
The pinned reference is go-quai **v0.57.0**
(`33db82f2891b267ba898ca38bd2dd8bf75cbb072`).

The shipping RPC executable serves a frozen imported read-only view. It is not
a validating or synchronizing node.

## Status and plan

[ROADMAP.md](ROADMAP.md) is the single plan and status file. Other files:

- [NETWORK_READINESS.md](NETWORK_READINESS.md): gate definitions.
- [PORTING_MAP.md](PORTING_MAP.md): Go-to-Rust coverage map, generated.
- [PERFORMANCE_LEDGER.md](PERFORMANCE_LEDGER.md): measured gains and regressions.
- [AGENTS.md](AGENTS.md): working rules.
- [docs/history/](docs/history/): per-step reports from earlier work, kept for
  reference only.

## Crate layout

| Crate | Role |
|---|---|
| `quai-primitives` | Work headers, transactions, Protobuf/RLP codecs, commitments, signatures |
| `quai-execution` | Intrinsic gas, Qi input checks, ETX queue roots, EVM frames and settlement |
| `quai-consensus` | ProgPoW/KAWPOW, auxiliary proofs, difficulty and PoEM rules |
| `quai-network` | Application framing, topics, optional private libp2p transport and a read-only public mode (`examples/mainnet-probe.rs`) |
| `quai-store` | Versioned materialized reads and the imported frozen view |
| `quai-rpc` | Frozen-view JSON-RPC service and profiling example |
| `quai-node` | Node binary; `quai-node replay` checks and executes mainnet blocks from a QEX1 export (see `ROADMAP.md`) |

See [the protocol notice](crates/quai-primitives/NOTICE.md) for attribution.

## Build and test

```sh
cargo build --release --locked --workspace
cargo test --locked --workspace
```

Python helpers build the distribution and run the per-area checks:

```sh
python3 scripts/build.py
python3 scripts/verify_rust.py
python3 scripts/verify_vm.py
python3 scripts/verify_stateful.py
python3 scripts/verify_zone_execution.py
python3 scripts/verify_private_branches.py
python3 scripts/verify_admission.py
python3 scripts/verify_confirmation.py
python3 scripts/verify_uncles.py
python3 scripts/verify_weakshares.py
python3 scripts/verify_canonical.py --peer
python3 scripts/verify_private_processes.py
python3 scripts/build_private_node.py
python3 scripts/verify_consensus.py
python3 scripts/verify_auxiliary.py
./bin/quai-protocol-check fixtures/protocol/work-headers.json
./bin/quai-protocol-check fixtures/protocol/transactions.json
./bin/quai-protocol-check fixtures/protocol/foundation.json
```

## Distribution and frozen RPC service

`dist/standalone/` contains the two Rust executables, capability metadata,
checksums and protocol attribution/license files. Previous mixed-profile
binaries are moved to `reference-artifacts/` when the build helper is first run;
they are excluded from the standalone distribution.

Run the frozen-view service with an explicit imported file:

```sh
./dist/standalone/quai-rpc --snapshot fixtures/native/scenario-a.json
```

This view is local and fixed. `/health` reports `standalone-rust`, zero Go runtime
dependency, and `chain_verified: false`. Unsupported state/simulation requests
return an availability error. Gateway/follower/native-probe flags are rejected
by the shipping binary. The product does not silently fall back to Go.

## SDK acceptance

Install the locked test dependencies and opt into the client test. Node/npm are
test tools, not runtime dependencies:

```sh
npm ci --prefix sdk --ignore-scripts --no-audit --no-fund
python3 scripts/verify_rust.py --sdk
```

The expected SDK values are previously recorded direct-Go outputs. No Go server
is started by these standalone tests.

## Profiling

Build the Rust-only development profiler and compare preserved binaries:

    cargo build --release --locked -p quai-rpc --example profile-rpc
    python3 scripts/profile_rpc.py --baseline /path/to/baseline/profile-rpc --candidate /path/to/candidate/profile-rpc

Every response is validated. This measures the handler without HTTP or
external client work. Allocation sampling runs separately and measures gross
requested bytes, not live memory. The example is excluded from distribution.

## Isolated reference lab

The Go-backed prototypes remain development tools behind the explicit
`quai-rpc/reference-gateway` Cargo feature. Build them into a separate source
copy; this does not overwrite the standalone binaries:

```sh
python3 scripts/build_reference.py --go-quai ../go-quai
cd reference-build
python3 scripts/verify_chain.py
python3 scripts/verify_next.py
```

Regenerating protocol vectors is also an explicit oracle task, never a build
step: `python3 scripts/gen_protocol_vectors.py --go-quai ../go-quai`.

Offline development helpers are explicit and excluded from the shipping bundle:

    python3 scripts/verify_foundation.py
    python3 scripts/verify_foundation.py --interop --go-quai ../go-quai

The second command starts only IPv4-loopback application-wire peers. It does
not connect to a public network or perform libp2p negotiation. Schema/vector
regeneration is also explicit: `scripts/gen_schema.py` and
`scripts/gen_foundation_vectors.py`, each with `--go-quai ../go-quai --check`
or `--write`. Default Cargo builds invoke neither Go nor protoc.
