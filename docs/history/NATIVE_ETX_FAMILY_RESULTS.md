# Composed execution for all defined ETX kinds

Native hierarchy preparation now composes all seven currently defined incoming ETX kinds (0 through 6) with signed Quai/Qi execution, append/header/body checks, authenticated context views and PCRC. The actual pinned Go oracle runs AppendHeader, ValidateBody, Process and ValidateState. Complete memory and persisted-state execution must match its commitments.

The fixed workload produces **4068 comparisons**, **24 distinct conditional blocks** across two overlapping branches, **246 Go-rejected valid-work mutations / 492 native rejected attempts**, **66 state roundtrips**, **16 writer reopens**, **100 context checks** and **2 fork-isolation checks**. Missing or mismatched context views and the narrower entry points reject this larger domain without publishing a head.

These inputs explicitly supply a height-259200 checkpoint. Its ancestry is unverified; these blocks add no genesis-to-head canonical coverage. Runtime profiles, default RPC boundary and disk codecs remain unchanged. The new component interface rejects unknown ETX kinds and retains the pre-controller/pre-fork bounds. Full global consensus and public canonical synchronization remain unfinished.

The complete source-bound regression runs **263 default and reduced Go-denied release tests**, **13 native transport tests**, fresh 8192-block replay, separate-process 8192-block sync, existing execution/state/reorg/recovery checks and frozen RPC/SDK comparisons. Go and protoc command attempts in the default qualification remain zero. Go is used only to generate the explicit offline reference corpus.

No runtime timing, heap or RSS improvement is measured by this correctness expansion. Existing proof-reuse operation counts remain separate from a measured end-to-end speedup.

Reproduce against the preserved official v0.57.0 source and completed corpora:

```sh
GOCACHE=/private/tmp/quai-benchmark-go-cache python3 scripts/gen_etx_family_vectors.py --go-quai ../go-quai --check
python3 scripts/verify_etx_family.py
python3 scripts/qualify_etx_family.py --small-oracle /path/to/completed-96-block-oracle --large-oracle /path/to/completed-8192-block-oracle --compatibility-baseline /path/to/preserved-compatible-source
```

The full genesis-to-activation oracle, native follower and dependent independent receiver remain isolated under their preserved earlier source. Their in-progress outputs grant no completed activation credit. Next work must integrate the wider domain with real ancestry, controllers/later forks, nonlocal hierarchy and the production coordinator.

Source-bound evidence: `results/etx-family-work-20261116T055952448924Z/summary.json`.
