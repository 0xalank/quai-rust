#!/bin/sh
# Export the pinned go-quai mainnet node (scripts/mainnet/go_node.sh) as QEX1
# (docs/mainnet-export-format.md) for the Rust replay (ROADMAP.md, Phase 1).
# The exporter (reference/mainnet-export/) is an explicit offline Go oracle,
# built into the pinned go-quai module with `go build -overlay`. Go is needed
# only here, never in the Rust build.
#
#   scripts/mainnet/export.sh [--live] [--max-zone N] [--out DIR] [--go-quai DIR] [--no-verify]
#
# Consistency
#
# The exporter never opens the node's own database. go-quai holds its LevelDB
# lock, and go-quai's LevelDB open runs a repair that writes to a database it
# finds damaged. The exporter reads an APFS clone (cp -c) instead: a clone
# takes seconds even for a large database and shares disk blocks until one
# side changes them.
#
# By default the script stops the node (SIGINT, clean shutdown), clones the
# prime, region-0 and zone-0-0 databases, starts the node again, and then
# exports from the clone while the node keeps syncing. The node is down only
# for the shutdown and the clone. This is the approach that gives a consistent
# canonical chain:
#   - cp -c clones one file after another. While the node runs, a LevelDB
#     compaction can end between two files, and the clone can then miss a
#     table file that its MANIFEST lists. go-quai's open repairs such a clone
#     by dropping the missing data.
#   - prime, region and zone are three databases. The node sets the canonical
#     head in prime, then in region, then in zone, so a clone of a running
#     node can hold a zone block without its prime or region entry, or the
#     reverse.
#   - With the node stopped, every file is final and the three databases
#     agree on one canonical chain.
#
# --live clones without stopping the node: zone first, then region, then
# prime, so that the dominant copies are the newer ones. The exporter detects
# an inconsistent copy (a missing canonical entry, a broken hash link, a
# dominant block that is stored but not canonical) and stops; then run the
# script again. Use --live only when the node must not stop.
#
# Resume
#
# The export directory keeps its complete 100,000-height segments. A new run
# checks their hashes, checks that their last blocks are still canonical,
# writes the last incomplete segment again and continues to the zone head of
# the new clone. A segment is written to <name>.pending and renamed when it is
# complete; the manifest is replaced after each segment.
#
# Environment:
#   QUAI_MAINNET_DIR  data root (default: ~/quai-data/mainnet-v0.57.0)
#   GO_QUAI_SRC       pinned go-quai checkout (default: ../go-quai)
#   STOP_TIMEOUT      seconds to wait for the node to stop (default: 600)
#   MIN_FREE_GB       the exporter stops below this free space (default: 30)
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
DATA=${QUAI_MAINNET_DIR:-"$HOME/quai-data/mainnet-v0.57.0"}
GO_QUAI=${GO_QUAI_SRC:-"$ROOT/../go-quai"}
OUT="$DATA/export"
NODE_SH=${GO_NODE_SH:-"$ROOT/scripts/mainnet/go_node.sh"}
STOP_TIMEOUT=${STOP_TIMEOUT:-600}
MIN_FREE_GB=${MIN_FREE_GB:-30}
MODE=stop
MAX_ZONE=-1
VERIFY=1

usage() {
	sed -n '8p' "$0" | sed 's/^# *//' >&2
	exit 2
}
while [ $# -gt 0 ]; do
	case "$1" in
	--live) MODE=live ;;
	--max-zone) [ $# -ge 2 ] || usage; MAX_ZONE=$2; shift ;;
	--out) [ $# -ge 2 ] || usage; OUT=$2; shift ;;
	--go-quai) [ $# -ge 2 ] || usage; GO_QUAI=$2; shift ;;
	--no-verify) VERIFY=0 ;;
	*) usage ;;
	esac
	shift
done
case "$MAX_ZONE" in -1 | [0-9]*) ;; *) usage ;; esac

log() { echo "export.sh: $*" >&2; }
die() { log "error: $*"; exit 1; }

PIN=$(sed -n 's/^REVISION="\([0-9a-f]*\)".*/\1/p' "$ROOT/scripts/reference_baseline.py")
[ -n "$PIN" ] || die "no REVISION in scripts/reference_baseline.py"
[ -d "$DATA/db/zone-0-0" ] || die "no node database at $DATA/db"
GO_QUAI=$(CDPATH= cd -- "$GO_QUAI" 2>/dev/null && pwd) || die "no go-quai checkout; set GO_QUAI_SRC or --go-quai"
[ "$(git -C "$GO_QUAI" rev-parse HEAD)" = "$PIN" ] || die "$GO_QUAI is not at the pinned revision $PIN"
git -C "$GO_QUAI" diff --quiet HEAD -- '*.go' '*.proto' go.mod go.sum || die "tracked go-quai sources in $GO_QUAI are modified"
mkdir -p "$OUT" "$DATA/logs" "$DATA/clones"
OUT=$(CDPATH= cd -- "$OUT" && pwd)

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
WORK=$(mktemp -d "${TMPDIR:-/tmp}/quai-mainnet-export.XXXXXX")
CLONE="$DATA/clones/export-$STAMP-$$"
LOG="$DATA/logs/export-$STAMP.log"
NODE_DOWN=0

cleanup() {
	status=$?
	if [ "$NODE_DOWN" = 1 ]; then
		log "starting the node again"
		"$NODE_SH" start >&2 || log "error: the node did not start; run $NODE_SH start"
	fi
	case "$CLONE" in "$DATA"/clones/export-*) rm -rf "$CLONE" ;; esac
	rm -rf "$WORK"
	exit $status
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# 1. Build the exporter into the pinned go-quai module. GOPROXY=off: only the
#    module cache is used; GOTOOLCHAIN=local: no toolchain download.
cat > "$WORK/overlay.json" <<EOF
{"Replace":{
 "$GO_QUAI/cmd/quai-mainnet-export/main.go":"$ROOT/reference/mainnet-export/main.go",
 "$GO_QUAI/core/rawdb/native_mainnet_export.go":"$ROOT/reference/mainnet-export/rawdb-export.go"}}
EOF
log "building the exporter in $GO_QUAI"
(cd "$GO_QUAI" && GOPROXY=off GOTOOLCHAIN=local go build -overlay "$WORK/overlay.json" \
	-ldflags "-X main.referenceRevision=$PIN" -o "$WORK/quai-mainnet-export" ./cmd/quai-mainnet-export) \
	> "$WORK/build.log" 2>&1 || { cat "$WORK/build.log" >&2; die "build failed"; }
BIN="$WORK/quai-mainnet-export"
{
	echo "date_utc $STAMP"
	echo "mode $MODE"
	echo "go_quai $GO_QUAI $PIN"
	echo "go $(go version)"
	shasum -a 256 "$ROOT/reference/mainnet-export/main.go" "$ROOT/reference/mainnet-export/rawdb-export.go" "$BIN"
} > "$LOG"

# 2. Clone the three context databases (same volume as the database).
clone_dbs() {
	mkdir "$CLONE" || die "cannot create $CLONE"
	for ctx in zone-0-0 region-0 prime; do
		cp -c -R "$DATA/db/$ctx" "$CLONE/" || die "cp -c failed: the clone must be on the APFS volume of $DATA/db"
	done
}
pid_file="$DATA/go-quai.pid"
node_running() { [ -f "$pid_file" ] && kill -0 "$(cat "$pid_file")" 2>/dev/null; }
if [ "$MODE" = live ]; then
	log "cloning the running node's databases (--live)"
	clone_dbs
elif node_running; then
	pid=$(cat "$pid_file")
	log "stopping the node (pid $pid) for a consistent clone"
	NODE_DOWN=1
	"$NODE_SH" stop >&2
	waited=0
	while kill -0 "$pid" 2>/dev/null; do
		if [ "$waited" -ge "$STOP_TIMEOUT" ]; then
			NODE_DOWN=0
			die "go-quai pid $pid did not stop within ${STOP_TIMEOUT}s; it is still shutting down. Start it with $NODE_SH start when it has stopped"
		fi
		sleep 1
		waited=$((waited + 1))
	done
	log "node stopped after ${waited}s; cloning"
	clone_dbs
	log "starting the node again"
	"$NODE_SH" start >&2 || log "error: the node did not start; run $NODE_SH start"
	NODE_DOWN=0
else
	log "the node is not running; cloning its databases"
	clone_dbs
fi
echo "clone $CLONE" >> "$LOG"

# 3. Export from the clone, then check the result again from its files.
#    The output (JSON lines, and errors) goes to the terminal and to $LOG.
run_logged() {
	{ "$@" 2>&1; echo $? > "$WORK/status"; } | tee -a "$LOG"
	return "$(cat "$WORK/status")"
}
log "exporting to $OUT (log: $LOG)"
run_logged "$BIN" -db "$CLONE" -out "$OUT" -max-zone "$MAX_ZONE" -min-free-gb "$MIN_FREE_GB" \
	|| die "export failed; see $LOG"
if [ "$VERIFY" = 1 ]; then
	log "verifying $OUT from its files"
	run_logged "$BIN" -verify -out "$OUT" || die "verify failed; see $LOG"
fi
log "done: $OUT"
