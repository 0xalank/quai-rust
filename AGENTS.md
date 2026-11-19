# Working on quai-rust

## Plan and status

- Work from `ROADMAP.md`. It is the single plan and status file.
- The measure of progress is the highest mainnet block that Rust replays with
  every root equal to the pinned go-quai v0.57.0. Phase 1 (mainnet replay) is
  the critical path.
- When work lands, tick its box and add one row to the execution log in
  `ROADMAP.md`. Do not write a new `*_RESULTS.md` report per step. Put the
  evidence in the commit message; keep raw run output in `results/`.
- A fail-closed limit is only correct when Go has the same limit. Do not add an
  execution limit that Go does not enforce in block validation.

## Boundaries

- The target is a native Rust Quai client. The current artifact is a frozen-view RPC service and protocol primitives; do not describe it as a validating or synchronizing node until full replay/sync gates pass.
- Keep Go out of the default build, distribution and runtime. Use Go only as an explicit offline oracle in the isolated reference profile. The mainnet go-quai node in `scripts/mainnet/` is such an oracle: it supplies replay data only.
- Preserve JSON-RPC IDs, errors, notifications and the read-only gateway boundary.
- Keep production network access, wallet keys and transaction submission out of
  benchmark and tuning scripts.
- Preserve attribution for imported or copied third-party code. Do not add
  tool signatures, generated-content badges or provenance marks to deliverables.

## Performance experiments

Performance work may run now (`ROADMAP.md`, Phase 4); every result must stay
identical to Go. Every header's proof-of-work is checked; no assume-valid.
When you run an experiment:

- Optimize Rust implementation files or the documented configuration space.
  During an experiment, keep the workload, Go reference/client, validation and
  reward contract fixed. Changing those requires a separate reviewed baseline.
- Rebuild and pass Rust plus differential/integration checks before timing a
  source change. Keep raw measurements and record source/binary hashes.
- Run timed workloads sequentially. Report shared-host noise and distinguish
  logical throughput, HTTP request latency, allocated bytes and sampled RSS.
