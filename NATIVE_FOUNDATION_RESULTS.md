# Native foundation results

Measured for correctness on 2026-11-09, using pinned go-quai
`db42af74d31f25390236ffeca14cca2cba7de639`. This is the local source profile,
including its five post-v0.56.1 fee-feature commits; no public-network fork
configuration is qualified. No throughput or memory benchmark was run.

## Implemented

- Checked-in Rust bindings for all 67 declared messages across five Protobuf
  schemas, including full work-object/body/view, auxiliary and peer envelopes.
  Raw field availability is separate from semantic acceptance. Default builds
  require neither Go nor protoc.
- Bounded canonical body-header/complete-work-object reconstruction and body
  commitments for zone/region/prime contexts. Transaction/outbound ETX and
  receipt list roots use a native Merkle Patricia implementation; manifest and
  interlink roots, body-header binding and uncle commitments are checked.
- Native receipt/log consensus RLP and the reference's 10,240-byte bloom.
  Raw-key and secure-key snapshot trie roots cover prefix/inline/hash boundaries.
- `quai-execution`: intrinsic Quai and Qi gas, Qi input ownership/locks/value/
  denomination/data/chain-ID checks, a separate unique-input invariant, and the
  live StateDB ETX queue's push/read/pop/counter/secure-root behavior in memory.
  ETX auxiliary economic entries are preserved without interpreting their rules.
- `quai-network`: bounded frames, typed envelopes, per-connection request
  ID/location/type correlation and exact topic construction. An explicit raw
  TCP loopback test exchanges messages through Go's common stream helpers.
  Libp2p transport/Noise/discovery/GossipSub/sync are not implemented.

## Evidence

| Check | Result |
| --- | ---: |
| Rust tests across five workspace crates | 34 passed |
| Existing bounded header/target + transaction comparisons | 4,343 passed |
| New schema/body/receipt/trie comparisons | 2,636 passed |
| Current protocol comparisons, total | 6,979 passed |
| New intrinsic-gas/Qi-input/live-ETX comparisons | 7,490 passed |
| Frozen RPC / two pinned SDK cases | 70 / 16 passed |
| Standalone runtime boundaries | 8 passed |
| Go/protoc-denied reduced-source build | Passed; zero attempts |
| Raw Rust–Go loopback request/response exchanges | 3 passed; oversized frame rejected |

The new protocol corpus contains 536 raw-schema cases, 90 body headers,
90 work objects (81 commitment-consistent and nine deliberately inconsistent
after normalization), 90 further tampered commitments, 144 rejected body-header
inputs, 48 ordered-list roots, 20 raw/secure snapshot roots, 132 receipts and eight
receipt-list roots. Prime, region and zone each have 27 positive commitment
cases. Synthetic commitment consistency is not proof/state/block validity.

The execution corpus contains 96 intrinsic-gas cases, 192 Qi input-helper cases
and 11 live StateDB ETX traces comprising 1,370 operations. ETX cases compare
roots and oldest/newest counters after push/read/pop, clone/revert and drain;
indices cross 127/128 and 255/256. Duplicate entries, preserved metadata,
failed native admission and float/overflow boundaries have explicit checks.
Twenty-four frozen Go topics are checked by the network Rust unit test.

The active source manifest, corpus and native binaries are hashed. Existing
registry dependency versions/checksums and historical timing contracts are
unchanged. Offline Go generation and raw loopback interoperability remain
explicit development tasks, excluded from the shipped client.

## Limits and unfinished work

Full EVM opcode/account execution, Quai-specific opcodes/precompiles/gas/state
rules, complete Qi output/value/fee/fork transitions and UTXO/MuHash mutations,
ETX hierarchy eligibility/conversions/rewards, auxiliary proof validation,
PoEM/fork choice, durable canonical storage and native libp2p sync remain open.
The shipping RPC service continues to serve an explicitly unverified frozen
view; new libraries do not silently promote imported data to canonical state.

The Qi input helper does not check signatures, full output economics or
transaction validity. Existing signature math is independently qualified.
Duplicate-input checking is a separate invariant; a block engine must also
isolate/consume UTXOs across transactions. ETX queue copying is not durable
rollback or a complete block-state journal. The trie implementation recomputes
bounded snapshots rather than maintaining a persistent incremental StateDB.

Prototype limits include 3 MiB messages/frames, 65,536 fields, 16,384 nested
messages, depth 32, 4,096 items per body list, 256-bit magnitudes, 64 MiB commitment
values and 32 MiB/16,384-entry ETX payload state with u64 indices. Protobuf groups
and active auxiliary semantic forms are unsupported. Qualify real protocol
limits before accepting live network blocks. Gas-scaling contexts must be
finite/nonnegative with uint64-safe products. Network/fork qualification
remains open; these bounds cannot be substituted for consensus rules.

The first generic sandbox run denied loopback listeners. The successful
integration run used explicit local-listener permission; no public peer,
wallet or transaction submission was involved. A fixture invariant also caught
noncanonical empty coinbases in synthetic uncle headers; the frozen corpus
uses explicit coinbases and requires positive cases in every chain context.

## Reproduce and inspect

    python3 scripts/build.py
    python3 scripts/verify_rust.py --sdk
    python3 scripts/verify_rust_build.py
    python3 scripts/verify_foundation.py --interop --go-quai ../go-quai
    python3 scripts/gen_schema.py --go-quai ../go-quai --check
    python3 scripts/gen_foundation_vectors.py --go-quai ../go-quai --check

The last two commands explicitly reproduce development artifacts; neither is
invoked by Cargo/default builds. Interoperability binds only IPv4 loopback.

[Foundation/interop summary](results/foundation-20261109T164250563401Z/summary.json),
[execution comparisons](results/foundation-20261109T164250563401Z/execution.json),
[protocol comparisons](results/foundation-20261109T164250563401Z/protocol.json),
[Go/protoc-denied build](results/foundation-20261109T164250563401Z/no-go-build.json),
[RPC/SDK acceptance](results/rust-20261109T164245847565Z/summary.json),
[corpus source hashes](fixtures/protocol/foundation-source.json),
[schema source hashes](fixtures/protocol/schema-source.json) and
[coverage map](PORTING_MAP.md) provide the evidence.
