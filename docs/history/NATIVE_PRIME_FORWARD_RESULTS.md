# Linked prime-forward economics

The installed source adds native supplied-history processing for the exact official prime forward-conversion branch: conversion output ordering and encoding, refund/repricing rules, forward exchange-rate update, rolling token-choice state, and the resulting output root. It reuses the already-qualified parent conversion/controller arithmetic while retaining the intermediate prices needed by the forward step.

The frozen corpus uses official go-quai v0.57.0 commit `33db82f2891b267ba898ca38bd2dd8bf75cbb072`. Qualification covers **256 histories and 1,024 linked forward steps**, with **23,040 matched ETX encodings**, **10,240 reference refunds**, 1,024 comparisons each for forward rate, choice sum, choice-window digest, output sequence and output root, plus 1,024 native header verifications. It rejects 1,024 binding mutations, 768 missing-input cases and 240 undefined zero-source cases. The persistent path qualifies 32 updates, 16 writer reopens and eight atomic aborts while copying at most 64 logical window rows and writing one changed row plus metadata.

This is a component result, not global append admission. The corpus supplies reviewed history and an initial one-write parent choice probe. Proof work, canonical ancestry, general coordinator integration, full append acceptance, later fork boundaries and public sync remain open. No timing, allocation, heap or RSS improvement is claimed.

The full source-bound qualification passes **281 release tests**, **13 native-transport tests** and **30 sequential gates**, including a fresh 8,192-block replay/receiver and old-current-old storage-format reopen. Default build/distribution/runtime use no Go. Go is only the explicit offline oracle.

Evidence:

- `results/prime-forward-20261116T192503387861Z/summary.json`
- `results/prime-forward-qualification-20261116T184306760849Z/summary.json`
- source build `7f42f8560bfc3c71be5d7f07f135eab4b19e9ca0541109713a13e37c5737940b`
- corpus `ff03ea1020cca016bfbd4bb406bf817eb511a8296e2514e9be0612f4be856fe4`

Full global consensus and canonical public synchronization remain unfinished; the tracked goal stays active.
