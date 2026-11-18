# Native prime economic controller arithmetic

Native Rust now implements the pinned conversion-flow EWMA, minimum and cap; miner-difficulty EWMA; KQuai discount; integer feedback; and historical exchange-rate resets, holds and freeze. Header miner-difficulty validation uses the shared implementation with 512-bit intermediates, sufficient for the 268-bit bound on 256-bit inputs.

The fixed corpus passes **3200 actual Go comparisons** across five arithmetic families. It includes controller activation and later fork boundaries, both rate directions, negative Euclidean division rounding, large magnitudes, minimum/cap order and feedback. Missing/zero historical rates and undefined feedback fail explicitly. Existing header and hierarchy results remain intact.

Default and reduced Go-denied builds pass **266 release tests / 209 Rust sources / 23 assets**; the transport feature passes **13 tests**. Fresh 8192-block replay, separate-process 8192-block sync and existing ETX/state/recovery/RPC checks pass against this exact source. Go and protoc default qualification command attempts remain zero.

These functions accept controlled history inputs. Except for the shared existing miner-difficulty path, complete block admission has not integrated the controller history and 4000-entry choice window. Runtime profiles and disk formats are unchanged. The all-ETX composition remains conditional on its explicitly supplied checkpoint. Full consensus and public canonical sync remain open.

No timing, heap or RSS improvement is measured. Reducing the miner EWMA intermediate width is an implementation change, not a proven end-to-end speedup.

```sh
GOCACHE=/private/tmp/quai-benchmark-go-cache python3 scripts/gen_controller_vectors.py --go-quai ../go-quai --check
python3 scripts/verify_controllers.py
python3 scripts/qualify_controllers.py --small-oracle /path/to/completed-96-block-oracle --large-oracle /path/to/completed-8192-block-oracle --compatibility-baseline /path/to/preserved-compatible-source
```

The long genesis producer/follower/receiver remains isolated under its original source; in-progress output grants no activation credit. A separate choice-window candidate has focused correctness evidence and must complete qualification and actual admission integration before it changes runtime scope.

Source-bound evidence: `results/controller-work-20261116T064028341659Z/summary.json`.
