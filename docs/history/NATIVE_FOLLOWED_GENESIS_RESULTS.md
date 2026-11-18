# Historical genesis-to-activation receiver

A preserved historical source build completed the long local activation-boundary run that was previously in progress. This evidence belongs to source build `3cc6807931c1f7e7aa255f97fea3602148ec351d0a4713bec8d8cf46b7ebc9e9`; it is not current-source qualification.

The native source replay validated **259,216 blocks from genesis**, including a sixteen-block signed-Quai suffix after height 259,200. It recorded **3,370,325 checks**, **405,025 context views** and **253 writer reopens**, with no checkpoint or supplied initial allocation import. The final head is `0x62fad16a70637ff4fbc198ff4f26ad7d17bfded16a1317171604638c801374ef`.

A separate native receiver imported the first **259,200 blocks** in 2,025 bounded windows, reopened at the exact boundary head, imported the sixteen-block suffix in one further window, then reopened at the final head with zero additional fetches. Prefix accounting records **405,002 requests**, **573,281,330 response-payload bytes**, **1,992,597 header-proof hits**, zero misses and a peak window of 128 blocks / 283,361 bytes. The suffix records 45 requests, 38,853 response-payload bytes, 121 hits, two misses and sixteen reused proofs. The harness passed **4,720 assertions** across separate OS processes and attempted no Go or protoc command.

These are logical request/payload/proof counters, not latency, throughput, allocation, heap or RSS measurements. The proof qualifies a local private profile through its signed-Quai activation suffix. It does not cover other ETX kinds, later forks, expansion, the general coordinator, full EVM behavior, automatic public-head discovery, public peers or production RPC. `full_consensus=false` and `canonical_public_sync=false` remain explicit.

Archived evidence: `results/followed-genesis-processes-20261116T102831936861Z/summary.json`.
