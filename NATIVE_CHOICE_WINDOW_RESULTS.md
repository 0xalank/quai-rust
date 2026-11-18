# Branch-bound native token-choice windows

The Rust controller now derives choices from actual ETXs and parent miner difficulty, retaining Go uint64 wrapping, signed Euclidean correction rounding, warmup/reset ordering, the full 4000-entry rolling window and historical rate feedback. Consecutive block/parent keys bind memory updates to one branch. Native persistence checks the exact previous tree root and metadata inside the same state transaction.

The fixed corpus passes **4216 Go window updates**, **25296 direct Go comparisons** and **33728 total checks**, with **8 writer reopens**, **2 fork-isolation checks** and **2 aborted/unpublished-parent rejections**. Matching complete-window digests bind 16864000 logical entries; those entries are not independent test assertions. Malformed row lengths, redundant zeros and aggregate mismatches cannot load or select the damaged version.

Observed logical bounds are **64 choice rows copied**, **one changed row persisted** and **one metadata record written per update**. The native window shares unchanged chunks and maintains its difficulty sum incrementally. These counts exclude Arc handles, trie nodes and database bookkeeping. No wall-time, allocated-byte, heap or RSS improvement is claimed.

Default and reduced Go-denied builds pass **268 release tests / 213 Rust sources / 23 assets**; native transport passes **13 tests**. Fresh 8192-block replay, independent-process 8192-block sync, existing execution/state/recovery/RPC/SDK and old/current-format checks pass against this exact source. Go/protoc command attempts in default qualification are zero.

The corpus explicitly supplies a pre-controller seed and controlled history. It grants no checkpoint/canonical authority; negative final choice difficulty remains outside this bounded component. This window is not yet integrated into complete native prime admission. Runtime profiles and disk formats are unchanged. Full global consensus and public canonical synchronization remain unfinished.

```sh
GOCACHE=/private/tmp/quai-benchmark-go-cache python3 scripts/gen_choice_window_vectors.py --go-quai ../go-quai --check
python3 scripts/verify_choice_window.py
python3 scripts/qualify_choice_window.py --small-oracle /path/to/completed-96-block-oracle --large-oracle /path/to/completed-8192-block-oracle --compatibility-baseline /path/to/preserved-compatible-source
```

The full 259216-block producer/follower/receiver remains frozen under its original source. Incomplete output grants no activation credit. The isolated all-ETX runtime candidate must complete its own qualification before changing installed runtime scope.

Source-bound evidence: `results/choice-window-work-20261116T070003552554Z/summary.json`.
