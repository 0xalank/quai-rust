#!/usr/bin/env bash
# Stress the test binaries that aborted with "pointer being freed was not
# allocated" (macOS 26.1-26.3 libmalloc false positive on uninitialized
# padding in `mpt::Link`; see ROADMAP.md, heap-crash). Each target runs
# --runs times, spread over --parallel workers; load raises the abort rate.
#
#   scripts/stress_tests.sh [--runs N] [--parallel P] [--only a,b] [--probe]
#                           [--malloc default|legacy] [--no-build]
#                           [--bin NAME=PATH]...
#
# Targets: replay (quai-node tests/replay.rs), precompiles (quai-execution
# tests/precompiles.rs), mpt (quai-store example mpt-first-pass: one trie pass
# over fixtures/protocol/state-db.json in a fresh process, MPT_REPS=3).
#
# --probe (macOS): load scripts/heap/free_probe.c into every run. It counts
# freed blocks whose first word carries libmalloc's free cookie in its upper
# 7 bytes ("near") or in all 8 ("exact"; such a block aborts on libmalloc
# 792.x). Before the fix every process had near hits (mpt about 40, replay
# about 140); after it, none. With --probe any near hit fails the run.
# --bin NAME=PATH runs a saved binary instead of building the target.
# --malloc legacy (macOS): MallocSecureAllocatorNano=0, the pre-xzone
# allocator, for comparison runs.
#
# Output: results/heap-crash/stress-<UTC time>/ (summary.txt, summary.json,
# per-target status files: run, exit code, probe near and exact counts; logs
# of failed runs). Exit 1 if any run failed or the probe counted any hit.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
runs=100
parallel=8
only="replay,precompiles,mpt"
probe=0
malloc=default
build=1
overrides=""
while [ $# -gt 0 ]; do
  case "$1" in
    --runs) runs="$2"; shift 2 ;;
    --parallel) parallel="$2"; shift 2 ;;
    --only) only="$2"; shift 2 ;;
    --probe) probe=1; shift ;;
    --malloc) malloc="$2"; shift 2 ;;
    --no-build) build=0; shift ;;
    --bin) overrides="$overrides
$2"; shift 2 ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

target_dir="${CARGO_TARGET_DIR:-$root/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
out="$root/results/heap-crash/stress-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$out"

# name|package|cargo selector|working directory|extra environment
targets=(
  "replay|quai-node|--test replay|crates/quai-node|"
  "precompiles|quai-execution|--test precompiles|crates/quai-execution|"
  "mpt|quai-store|--example mpt-first-pass|crates/quai-store|MPT_REPS=3"
)

selected() { [[ ",$only," == *",$1,"* ]]; }

override_of() { # name -> PATH given with --bin NAME=PATH, if any
  echo "$overrides" | sed -n "s|^$1=||p" | tail -1
}

binary_of() { # package selector -> path of the built binary
  local package="$1" selector="$2" path
  cd "$root"
  if [[ "$selector" == --example* ]]; then
    local name="${selector#--example }"
    if [ "$build" = 1 ]; then
      nice -n 10 cargo build --release --locked -p "$package" --example "$name" >&2
    fi
    echo "$target_dir/release/examples/$name"
  else
    local log
    log="$(nice -n 10 cargo test --release --locked --no-run -p "$package" $selector 2>&1)" ||
      { echo "$log" >&2; return 1; }
    path="$(echo "$log" | sed -n 's/.*Executable .*(\(.*\)).*/\1/p' | tail -1)"
    case "$path" in /*) echo "$path" ;; *) echo "$root/$path" ;; esac
  fi
}

probe_counts() { # log -> "near exact" (the summary is missing after an abort)
  local n e
  read -r n e < <(sed -n 's/^FREEPROBE summary near 0x\([0-9a-f]*\) exact 0x\([0-9a-f]*\).*/\1 \2/p' "$1"; echo)
  if [ -n "$n" ]; then
    echo "$((16#$n)) $((16#$e))"
  else
    echo "$(grep -c '^FREEPROBE near ptr' "$1") $(grep -c '^FREEPROBE exact ptr' "$1")"
  fi
}

envs=(MallocDebugReport=stderr)
case "$malloc" in
  default) ;;
  legacy) envs+=(MallocSecureAllocatorNano=0) ;;
  *) echo "--malloc must be default or legacy" >&2; exit 2 ;;
esac

if [ "$probe" = 1 ]; then
  if [ "$(uname -s)" != Darwin ]; then
    echo "--probe needs macOS" >&2; exit 2
  fi
  clang -O2 -dynamiclib -o "$out/free_probe.dylib" "$root/scripts/heap/free_probe.c"
  envs+=("DYLD_INSERT_LIBRARIES=$out/free_probe.dylib")
fi

worker() { # name bin dir extra first last
  local name="$1" bin="$2" dir="$3" extra="$4" first="$5" last="$6" i code
  cd "$root/$dir"
  for ((i = first; i <= last; i++)); do
    local log="$out/$name-$i"
    env "${envs[@]}" $extra "$bin" > "$log.out" 2> "$log.err" && code=0 || code=$?
    local near=0 exact=0
    if [ "$probe" = 1 ]; then
      read -r near exact < <(probe_counts "$log.err")
    fi
    echo "$i $code $near $exact" >> "$out/$name.status"
    if [ "$code" = 0 ] && [ "$near" -le 0 ] && [ "$exact" -le 0 ]; then rm -f "$log.out" "$log.err"; fi
  done
}

failed=0
{
  echo "host: $(uname -srm); $(sw_vers -productVersion 2>/dev/null || true); load: $(uptime | sed 's/.*averages*: //')"
  echo "runs: $runs per target, parallel: $parallel, malloc: $malloc, probe: $probe"
} > "$out/summary.txt"
json="["
for entry in "${targets[@]}"; do
  IFS='|' read -r name package selector dir extra <<< "$entry"
  selected "$name" || continue
  given="$(override_of "$name")"
  if [ -n "$given" ]; then
    bin="$(cd "$root" && cd "$(dirname "$given")" && pwd)/$(basename "$given")"
  else
    bin="$(binary_of "$package" "$selector")"
  fi
  [ -x "$bin" ] || { echo "no binary for $name" >&2; exit 1; }
  shasum -a 256 "$bin" | sed "s|  .*| $name|" >> "$out/binaries.sha256"
  start=$(date +%s)
  per=$(( (runs + parallel - 1) / parallel ))
  pids=()
  for ((w = 0; w < parallel; w++)); do
    first=$(( w * per + 1 )); last=$(( (w + 1) * per ))
    [ "$last" -gt "$runs" ] && last=$runs
    [ "$first" -gt "$last" ] && break
    worker "$name" "$bin" "$dir" "$extra" "$first" "$last" & pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p"; done
  secs=$(( $(date +%s) - start ))
  total=$(wc -l < "$out/$name.status" | tr -d ' ')
  ok=$(awk '$2 == 0' "$out/$name.status" | wc -l | tr -d ' ')
  aborts=$(awk '$2 == 134' "$out/$name.status" | wc -l | tr -d ' ')
  other=$(( total - ok - aborts ))
  near=$(awk '$3 > 0 {s += $3} END {print s + 0}' "$out/$name.status")
  near_runs=$(awk '$3 > 0' "$out/$name.status" | wc -l | tr -d ' ')
  exact=$(awk '$4 > 0 {s += $4} END {print s + 0}' "$out/$name.status")
  line="$name: $total runs, $ok ok, $aborts aborts (SIGABRT), $other other failures, ${secs}s"
  [ "$probe" = 1 ] && line="$line; probe near $near in $near_runs runs, exact $exact"
  echo "$line" | tee -a "$out/summary.txt"
  json="$json{\"target\":\"$name\",\"runs\":$total,\"ok\":$ok,\"aborts\":$aborts,\"other\":$other,\"seconds\":$secs,\"probe\":$probe,\"near\":$near,\"near_runs\":$near_runs,\"exact\":$exact},"
  if [ "$ok" != "$total" ] || [ "$near" != 0 ] || [ "$exact" != 0 ]; then failed=1; fi
done
echo "${json%,}]" > "$out/summary.json"
echo "results: $out"
exit "$failed"
