# Native serial VM results

The execution library now runs bounded isolated Quai VM frames entirely in Rust.
This increment implements account/storage roots and 135 opcode values. It does
not execute complete transactions or validate/synchronize a chain. The frozen
RPC server still exposes only its previous eight read methods.

## Verified results

| Check | Result |
| --- | ---: |
| Native workspace tests | 42 passed |
| Frozen VM frames | 1,658 |
| Supported opcode values observed in matched traces | 135 / 135 |
| Instruction steps included in matched trace digests | 16,620 |
| Separate state-gas scaling cases | 3,872 |
| VM/root/gas/refund/log/trace comparisons | 37,747; zero mismatches |
| Frame outcomes | 1,564 success, 15 revert, 79 fault |
| Existing protocol comparisons | 6,979 |
| Existing execution-helper/live-ETX comparisons | 7,490 |
| Frozen RPC / pinned SDK cases | 70 / 16 |

[Machine VM result](results/vm-20261109T175203664406Z/summary.json) and
[RPC/SDK acceptance](results/rust-20261109T175209455918Z/summary.json) bind results to the current
source/build hashes. The existing raw application-wire loopback test is rerun
separately; it does not establish native libp2p transport. The reduced source
build denies Go, gofmt and protoc, with no Go checkout present.

## Implemented contract

The native frame handles fixed-width arithmetic including signed operations
and 512-bit modular intermediates; stack, memory, jumps and copy operations;
calldata/environment queries in the declared context; SLOAD/SSTORE; LOG0–4;
RETURN/REVERT; transient storage; MCOPY/PUSH0 and their height gate; and Quai
ISADDRINTERNAL. Storage gas/refunds use Quai's exact integer state-size
scaling, including pinned BinaryLog normalization, rather than floating-point
approximations. Access-list address checks and warm/cold slot costs follow
the pinned implementation. Mandatory access lists alone do not make the
transaction dependency graph complete.

Account commitments use Quai's five fields: nonce, balance, secure storage
root, code hash and storage size. Successful frames commit an in-memory
snapshot; faults, reverts, unsupported operations and resource limits preserve
the original world. Original storage, refund and transient state start fresh
for each invocation. This is an isolated top-level frame API, not a nested-call
or multi-transaction API.

Matched trace digests include PC/opcode, gas, state gas, refund, complete stack
and memory hash at each recorded instruction. Before/after roots, output,
error class, remaining gas, storage values/size and logs are also checked.
Independent tests cover abort after a storage write, snapshot/storage limits,
step/trace/memory limits, read-only transient writes, transient reset and
storage-size overflow. Resource and capability limits return an unsupported
execution-domain error, rather than asserting protocol invalidity.

Supported snapshot limits are 4,096 accounts, 4,096 slots per account, 65,536
aggregate slots and 32 MiB code/storage budget; a frame allows 32,768-byte code,
128 KiB input, 1 MiB memory, 200,000 steps and 64 MiB cumulative trace work.
These prototype limits are not qualified consensus limits. Full protocol
domains and production resource budgets remain open.

## Reference and remaining work

The offline oracle is go-quai
`db42af74d31f25390236ffeca14cca2cba7de639`.
[Source evidence](fixtures/protocol/serial-vm-source.json) pins the Go inputs,
oracle/driver and mathutil dependency. The generator uses real committed Go
StateDB tries. It prepares the transaction before populating its access list.
The corpus includes successful and reverted writes, missing access-list
addresses, warm/cold slots, varied trie sizes, arithmetic boundaries, stack/
memory faults, jump destinations and before/at/after new-op activation.

Fourteen valid opcode values remain unsupported, including CALL/CREATE
variants, external account/code queries, BLOCKHASH, ETX/CONVERT and
SELFDESTRUCT. There are no precompiles, value transfers, nonce/fee settlement,
full Qi output/fee/UTXO transitions, economic ETX eligibility, full consensus
or durable canonical state. There is no public-network access or new speed/
allocation benchmark in this increment. [The coverage map](PORTING_MAP.md)
marks EVM/account state partial and keeps G3 open as a full-client gate.

Next: implement a nested-frame journal and call/create lifecycle, then
precompiles and complete transaction settlement. Preserve original storage,
refund, transient/access state and logs at the correct rollback boundaries.
Follow with combined Quai/Qi/ETX full-block replay, proof/hierarchy consensus
and durable state. The native libp2p transport/sync work remains a separate
open package.

## Reproduce

Native checks require no Go toolchain or node:

```sh
python3 scripts/build.py
python3 scripts/verify_vm.py
python3 scripts/verify_rust.py --sdk
python3 scripts/verify_rust_build.py
```

The explicit offline oracle regeneration is separate:

```sh
python3 scripts/gen_vm_vectors.py --go-quai ../go-quai --check
```

Frozen expected outputs and historical benchmark contracts were preserved.
Third-party copyright/license notices are retained in the crate and standalone
bundle. Visible text, binary strings and readable metadata are inspected for
AI-identifying marks before handoff; no removal of unknown invisible marks is
claimed. See the final verification record beside the machine VM result.
