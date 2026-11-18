# Optimization contract

Objective: improve the measured RPC workload while preserving accepted protocol
semantics and respecting resource limits. This experiment is a starting point
for code optimization and later policy learning, not a whole-node benchmark.

## State, action and reward

The current state is the observed throughput, request latency, failures and
sampled server RSS for fixed profiles. An action selects one of six bounded
configurations: value/raw fixture encoding, batch concurrency 1/4/8, and Tokio
worker count 2/4. UCB1 chooses the next action after one initial trial per arm.

Reward is the mean natural logarithm of the candidate/baseline throughput ratio
across the three training profiles, clipped to [-2, 2]. A failed correctness
check, failed request, missed memory measurement, sampled RSS over 256 MiB, or
request p95 greater than `2 * baseline_p95 + 5 ms` rejects the trial with -2.
This latency threshold is a coarse experimental guard, not a production SLA.

The selected arm has the highest mean training reward. It is recommended only
if two holdout rounds pass the guards and both have a geometric mean throughput
ratio above 1.03. Those are observed checks, not a statistical significance test.
No default server setting is changed by the tuner.

## Reproducibility

`fixtures/contract-sha256.json` records the workload, reference/client, validation
and scoring sources. `scripts/tune.py` checks it before running. Build metadata
also records the Rust source and executable hashes, lockfile and Go reference
revision, preventing an old binary from silently being scored after edits.

Run `scripts/seal.py` only after reviewing an intentional change to the benchmark
contract. Do not reseal it to hide a correctness or performance regression.
Each experiment retains raw measurements, both sides of every comparison, load
averages and the exact chosen configurations. Trials are sequential.

## Extending to source optimization or reinforcement learning

The server is deliberately a small library/binary boundary. A future controller
can propose a source patch, build it, run the fixed checks, and consume the same
measurement/reward records. It must reject invalid variants before timing and
record the source revision. Keep test workloads and validators outside the
controller's edit set. Never reward skipped validation, stale state, dropped
requests, changed gas/consensus rules, or reduced test coverage.

Before training a more general policy, replace toy fixture traffic with a
representative, frozen trace; choose explicit throughput/latency/memory targets;
and keep a fresh validation trace out of policy selection. The current bandit
does not train a language model, execute arbitrary generated code or contact
external services.

## Native read measurements

`scripts/benchmark_native.py` provides a separate source-recorded baseline for
real-node and typed-scenario reads. It does not change this sealed bandit contract
or automatically promote a production configuration. Use its total resource and
per-method measurements when defining a future realistic optimization reward.
