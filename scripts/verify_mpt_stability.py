#!/usr/bin/env python3
"""Exercise optimized trie allocation and destruction in fresh OS processes."""
import datetime
import json
import os
from pathlib import Path
import platform
import subprocess

from lab import ROOT, save, sha

PROCESSES = 8
REPETITIONS_PER_PROCESS = 256
UPDATES_PER_REPETITION = 1_512
MATERIALIZATIONS_PER_REPETITION = 33
MARKER = (
    "MPT_DROP_STRESS repetitions=256 updates=387072 "
    "materializations=8448"
)

stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
out = ROOT / "results" / ("mpt-stability-" + stamp)
out.mkdir()
env = {**os.environ, "CARGO_NET_OFFLINE": "true"}
command = [
    "cargo",
    "test",
    "--release",
    "--locked",
    "--offline",
    "-p",
    "quai-store",
    "--test",
    "mpt_drop_stress",
    "--",
    "--nocapture",
]
runs = []
for number in range(1, PROCESSES + 1):
    result = subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    log = out / f"run-{number}.log"
    log.write_text(result.stdout)
    assert result.returncode == 0, log
    assert MARKER in result.stdout, log
    runs.append(
        {
            "process": number,
            "exit_code": result.returncode,
            "log": str(log.relative_to(ROOT)),
            "log_sha256": sha(log),
        }
    )

report = {
    "passed": True,
    "build_sha256": sha(ROOT / "results/rust-build.json"),
    "reference_revision": "33db82f2891b267ba898ca38bd2dd8bf75cbb072",
    "release_profile": "thin-lto-codegen-units-1",
    "fresh_processes": PROCESSES,
    "repetitions": PROCESSES * REPETITIONS_PER_PROCESS,
    "path_copy_updates": PROCESSES * REPETITIONS_PER_PROCESS * UPDATES_PER_REPETITION,
    "materialization_reloads": PROCESSES
    * REPETITIONS_PER_PROCESS
    * MATERIALIZATIONS_PER_REPETITION,
    "mpt_source_sha256": sha(ROOT / "crates/quai-primitives/src/mpt.rs"),
    "stress_source_sha256": sha(ROOT / "crates/quai-store/tests/mpt_drop_stress.rs"),
    "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "host": platform.platform(),
    "runs": runs,
    "runtime_go_dependency": False,
    "go_commands_attempted": 0,
    "timing_benchmark": False,
    "heap_or_rss_measured": False,
    "full_consensus": False,
    "canonical_public_sync": False,
}
save(out / "summary.json", report)
save(
    ROOT / "results/latest-mpt-stability.json",
    {"passed": True, "directory": str(out.relative_to(ROOT))},
)
print(json.dumps(report, indent=2))
