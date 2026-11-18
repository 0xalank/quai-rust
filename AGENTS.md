# Working on quai-rust

- The target is a native Rust Quai client. The current artifact is a frozen-view RPC service and protocol primitives; do not describe it as a validating or synchronizing node until full replay/sync gates pass.
- Keep Go out of the default build, distribution and runtime. Use Go only as an explicit offline oracle in the isolated reference profile.
- Preserve JSON-RPC IDs, errors, notifications and the read-only gateway boundary.
- Keep production network access, wallet keys and transaction submission out of
  benchmark and tuning scripts.
- Optimize Rust implementation files or the documented configuration space.
  During an experiment, keep the workload, Go reference/client, validation and
  reward contract fixed. Changing those requires a separate reviewed baseline.
- Rebuild and pass Rust plus differential/integration checks before timing a
  source change. Keep raw measurements and record source/binary hashes.
- Run timed workloads sequentially. Report shared-host noise and distinguish
  logical throughput, HTTP request latency, allocated bytes and sampled RSS.
- Preserve attribution for imported or copied third-party code. Do not add
  tool signatures, generated-content badges or provenance marks to deliverables.
