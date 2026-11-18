# Offline local-zone admission oracle

The reference is Dominant Strategies go-quai v0.57.0 revision
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`. Preserve its retained GPL-3.0-only
license and attribution.

Two branches share heights zero through eight and differ thereafter. They use
native full-size ProgPoW and actual CalcOrder, AppendHeader, ValidateBody,
StateProcessor.Process, ValidateState, PCRC and manifest calculation. Both
engine slots use the native engine; there is no injected order or fake engine.
The offline nonce search selects local zone order, so higher-order confirmation
is outside this corpus. Actual CollectNewlyConfirmedEtxs returns an empty list
for this local-zone segment.

The separately pinned private-admission-v1.json uses difficulty two and observes
the pre-transaction gas/state schedule. The older private-native-v1 profile and
its execution-only vectors are preserved. They intentionally do not qualify
header validity.

Thirty-one mutations retain real valid work and change individual header,
manifest, fee or execution-commitment fields. Each mutation is round-tripped
through Go Protobuf decoding before validation. All are rejected by Go. The
adapter exports termini through their accessors because the upstream generated
JSON marshal method copies into nil destination slices.

Use scripts/gen_admission_vectors.py --go-quai ../go-quai --write/--check only
as an explicit offline oracle. Default Rust builds/tests/runtime never run Go.
This corpus does not establish complete higher-order, workshare, fork, canonical
selection or public-network qualification.
