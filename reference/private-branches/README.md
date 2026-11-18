# Offline private branch oracle

This adapter follows Dominant Strategies go-quai v0.57.0 revision
`33db82f2891b267ba898ca38bd2dd8bf75cbb072`, under the retained GPL-3.0-only
terms and upstream attribution. It runs actual StateProcessor.Process on two
sixteen-block private chains. Heights zero through eight are identical;
later signed storage writes differ. Full-size native ProgPoW work, rewards,
incoming coinbases, roots and typed receipt bytes are recorded.

Order/hierarchy context and inbound eligibility are controlled adapter inputs.
This does not establish full block/header/hierarchy admission or PoEM canonical
selection. The native private journal stores execution-checked candidates and
requires an explicit private head choice. Public RPC remains frozen/unverified.

Regeneration is explicit and offline:

    python3 scripts/gen_private_branch_vectors.py --go-quai ../go-quai --write
    python3 scripts/gen_private_branch_vectors.py --go-quai ../go-quai --check

Ordinary Rust builds, tests and the standalone runtime never invoke Go.
