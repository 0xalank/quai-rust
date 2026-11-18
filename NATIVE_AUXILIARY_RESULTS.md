# Signed auxiliary proof admission

The native consensus library now verifies the signed auxiliary stage for
supported KAWPOW, BTC, BCH and Litecoin/Scrypt payloads: template hashing,
the pinned 2-of-3 MuSig2 quorum, coinbase inputs, signature time, Quai seal
commitment, donor Merkle root and Litecoin/Dogecoin two-chain commitment.
Supported signed KAWPOW block headers proceed to the native light engine.

This is a component admission stage. Full header/uncle/history/economic
validation, canonical fork choice and independent sync remain incomplete.
Full block admission still returns unsupported; RPC remains a frozen,
unverified imported view.

## Results

| Verification | Result |
| --- | ---: |
| Auxiliary comparisons | 6,503; zero mismatches |
| Semantic auxiliary codec cases | 224 |
| Signed-template cases | 476 |
| Signature-valid template cases | 87 |
| Binding/role cases | 560 |
| Binding-valid cases | 168 |
| Binding-valid cases under pinned protocol keys | 18 |
| Integrated signed KAWPOW proof cases | 2 |
| Workspace Rust tests | 54 passed |

[Auxiliary machine result](results/auxiliary-20261109T191838477483Z/summary.json) records build/source hashes.
The two KAWPOW cases use the pinned quorum and an explicit public lab quorum.
Each checks complete native proof/header/work-object binding, rejects an
altered mix and signature, and retains `fully_validated: false`. The pinned
runtime rejects the lab signature before generating a proof cache.

Existing suites pass against the current build:
[6,461 consensus comparisons](results/consensus-20261109T191846610309Z/summary.json),
[37,747 VM comparisons](results/vm-20261109T191855833942Z/summary.json),
[70 RPC and 16 SDK cases](results/rust-20261109T191842404553Z/summary.json), 6,979 protocol comparisons and
7,490 execution-helper/live-ETX comparisons. Three raw application-wire
requests are exchanged over loopback; no libp2p transport or public peers
are involved. The Go/protoc-denied reduced build runs all 54 tests and both
complete native consensus/auxiliary checkers without oracle sources or a Go
checkout. No throughput, latency or memory benchmark was run.

## Signed message and trust boundary

Template hashes use the canonical reconstructed Protobuf with signatures
excluded. BTC/BCH version bits are masked with `0xe0000000` before signing,
matching ASICBoost handling. Byte-field presence, including empty signatures,
is preserved according to the pinned Go decode/re-encode behavior. Unknown
fields and the unused auxiliary signature-time field are dropped during
semantic reconstruction. Donor headers retain the consumed 80/120 bytes.

The verifier tries all six ordered two-signer pairs from three trusted
compressed public keys. The default keys are exactly the pinned protocol
keys. Incoming proofs contain no quorum configuration. Custom quorum
construction is explicitly a trusted chain-spec operation. The offline lab
uses public deterministic scalars 1, 2 and 3 and exercises every signer pair
in both orders for every supported proof ID; it cannot qualify a production
network or change the default trust set.

An exact 64-byte signature check precedes the curve parser. Tests cover every
length from 0 through 130, malformed public keys, oversized branches,
truncated coinbases, malicious lengths and untrusted donor heights. Signature
checks run before the KAWPOW epoch cache is selected or generated.

## Binding contract

The coinbase must have one input, zero previous txid, `0xffffffff` index and
sequence. The supported direct-push script yields the height, merged-mining
magic/commitment, skipped extranonce push and four-byte signature time.
Donor and Quai times cannot precede that time. The coinbase output tail is
signed exactly as extracted by Go; it is not independently interpreted as
proof of donor-chain transaction validity.

For KAWPOW/BTC/BCH the commitment matches the native Quai seal. For Scrypt it
matches the exact reversed two-leaf Dogecoin/Quai root, with nonzero 32-byte
Dogecoin hash, tree size 2 and nonce 0. The coinbase and branch recompute the
donor Merkle root. The extracted template then verifies against the trusted
quorum. The same stage applies block versus workshare PoW-ID fork policy;
BTC/BCH/Scrypt share bindings do not become valid block proofs.

Mutations cover signed fields, masked/unmasked version bits, payouts, branch
entries, coinbase prevout/index, time, Quai data/seal, auxiliary hash and
signatures. Miner-flexible nonce/coinbase version/extranonce changes preserve
the intended Go behavior when the donor Merkle root is updated. These are
component fixtures, not accepted network blocks.

Explicit auxiliary-aware header/work-object codecs extend the library while
the previous default decoder retains its frozen unsupported-domain behavior.
This preserves the earlier corpus and benchmark contracts. The new path
canonicalizes auxiliary headers and hashes with actual auxiliary Protobuf
commitments, then verifies signatures/bindings and native KAWPOW work.

Prototype resource limits are 2 MiB auxiliary messages, 1 MiB coinbases, 64
branch entries and 4 KiB per branch item; outer schema budgets still apply.
These are bounded supported domains, not qualified production consensus
limits. Merkle siblings follow Go's actual padded/truncated 32-byte copy
behavior. Independent donor-chain validation is not claimed.

## Remaining consensus work

Full header/parent/time/number/location/coinbase/lock, gas/state/fee/controller
policy still needs integration. Uncle/workshare count, target, ancestry,
classification, distance, pre-fork discount and hierarchy eligibility remain
open. Genesis/expansion policy, termini/interlinks, competing branches,
complete economic state replay and durable canonical publication are still
required. G4 remains partial and the network target remains unqualified.
See [coverage](PORTING_MAP.md) and [network gates](NETWORK_READINESS.md).

## Reproduce

The offline reference is go-quai
`db42af74d31f25390236ffeca14cca2cba7de639`.
[Source evidence](fixtures/protocol/auxiliary-source.json) pins the generator,
core validation helpers, MuSig2 implementation, parameters and proof engine.
It uses actual Go template/signature/extraction/Merkle helpers in the order of
the auxiliary validation stage; it does not invoke full header/state acceptance.
Generation is deterministic, publishes parsed output atomically and keeps
expected invalid-signature diagnostics on stderr.

```sh
python3 scripts/build.py
python3 scripts/verify_auxiliary.py
python3 scripts/verify_consensus.py
python3 scripts/verify_vm.py
python3 scripts/verify_rust.py --sdk
python3 scripts/verify_rust_build.py
```

The optional oracle remains separate from build/runtime:

```sh
python3 scripts/gen_auxiliary_vectors.py --go-quai ../go-quai --check
```

The final verification record beside the machine result checks installed
source/binary hashes, historical fixtures, bundle checksums, licenses,
readable text/metadata and executable strings. Required third-party notices
and technical provenance are preserved; no removal of unknown invisible
marks is claimed.
