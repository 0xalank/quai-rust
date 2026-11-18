#!/bin/sh
# One pass of the mainnet replay loop (ROADMAP.md, Phase 1):
#   1. extend the QEX1 export: scripts/mainnet/export.sh --live (it resumes);
#   2. continue the Rust replay from its latest checkpoint:
#      quai-node replay --resume --state;
#   3. append the result line and the highest passed zone height to
#      results/mainnet-replay/history.jsonl, and print the measure.
# A lock directory keeps two passes from overlapping.
#
#   scripts/mainnet/replay_loop.sh [--no-export]
#
# The measure is the highest mainnet zone height that the Rust replay passed
# with every root equal to go-quai v0.57.0.
#
# Environment:
#   QUAI_MAINNET_DIR   data root (default: ~/quai-data/mainnet-v0.57.0)
#   QUAI_NODE_DATADIR  quai-node data directory (default: $QUAI_MAINNET_DIR/quai-node)
#   REPLAY_JOBS        proof-of-work threads (default: 3)
#   CARGO_BUILD_JOBS   build jobs (default: 3)
#   GO_QUAI_SRC        pinned go-quai checkout for export.sh (its default is
#                      ../go-quai next to this repository; set it in a worktree)
set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
DATA=${QUAI_MAINNET_DIR:-"$HOME/quai-data/mainnet-v0.57.0"}
NODE_DATA=${QUAI_NODE_DATADIR:-"$DATA/quai-node"}
JOBS=${REPLAY_JOBS:-3}
EXPORT_DIR="$DATA/export"
HISTORY_DIR="$ROOT/results/mainnet-replay"
HISTORY="$HISTORY_DIR/history.jsonl"
LOCK="$DATA/replay-loop.lock"
BIN="$ROOT/target/release/quai-node"
DO_EXPORT=1

case "${1:-}" in
"") ;;
--no-export) DO_EXPORT=0 ;;
*) sed -n '10p' "$0" | sed 's/^# *//' >&2; exit 2 ;;
esac

log() { echo "replay_loop.sh: $*" >&2; }

# Lock: mkdir is atomic. A lock whose process is gone is stale.
mkdir -p "$DATA" "$HISTORY_DIR" "$DATA/logs"
if ! mkdir "$LOCK" 2>/dev/null; then
	other=$(cat "$LOCK/pid" 2>/dev/null || true)
	if [ -n "$other" ] && kill -0 "$other" 2>/dev/null; then
		log "another pass is running (pid $other)"
		exit 3
	fi
	log "removing the stale lock of pid ${other:-unknown}"
	rm -rf "$LOCK"
	mkdir "$LOCK" || { log "cannot take the lock $LOCK"; exit 3; }
fi
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT
trap 'exit 130' INT TERM

STAMP=$(date -u +%Y%m%dT%H%M%SZ)
START_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)

log "building quai-node"
(cd "$ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3} cargo build --release --locked -p quai-node) >&2 \
	|| { log "build failed"; exit 2; }

EXPORT_STATUS=skipped
if [ "$DO_EXPORT" = 1 ]; then
	log "extending the export (export.sh --live)"
	"$ROOT/scripts/mainnet/export.sh" --live >&2
	EXPORT_STATUS=$?
	[ "$EXPORT_STATUS" = 0 ] || log "export.sh failed ($EXPORT_STATUS); replaying the export as it is"
fi
[ -f "$EXPORT_DIR/manifest.json" ] || { log "no export at $EXPORT_DIR"; exit 2; }

OUT="$DATA/logs/replay-$STAMP.out"
ERR="$DATA/logs/replay-$STAMP.log"
log "replaying (log: $ERR)"
/usr/bin/time -l "$BIN" --datadir "$NODE_DATA" replay --export "$EXPORT_DIR" \
	--resume --state --jobs "$JOBS" > "$OUT" 2> "$ERR"
REPLAY_STATUS=$?

RESULT=$(grep '^replay ' "$OUT" | tail -n 1)
REPORT="$NODE_DATA/replay/last.json"
field() { sed -n "s/^ *\"$1\": \\([0-9.a-z]*\\),*$/\\1/p" "$REPORT" 2>/dev/null | head -n 1; }
HIGHEST=$(field highest_passed_zone_height)
LAST_ZONE=$(field export_last_zone)
RATE=$(field zone_blocks_per_second)
RUN_HEIGHTS=$(field heights_this_run)
RSS=$(sed -n 's/^ *\([0-9][0-9]*\) *maximum resident set size.*/\1/p' "$ERR" | tail -n 1)
escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
printf '{"utc":"%s","export_status":"%s","replay_status":%s,"highest_passed_zone_height":%s,"export_last_zone":%s,"heights_this_run":%s,"zone_blocks_per_second":%s,"peak_rss_bytes":%s,"result":"%s"}\n' \
	"$START_UTC" "$EXPORT_STATUS" "$REPLAY_STATUS" "${HIGHEST:-null}" "${LAST_ZONE:-null}" \
	"${RUN_HEIGHTS:-null}" "${RATE:-null}" "${RSS:-null}" "$(escape "$RESULT")" >> "$HISTORY"

echo "$RESULT"
if [ "$REPLAY_STATUS" -le 1 ] && [ -n "${HIGHEST:-}" ] && [ "$HIGHEST" != null ]; then
	echo "measure: mainnet zone $HIGHEST replayed with every root equal to go-quai v0.57.0 (export ends at ${LAST_ZONE:-?}; ${RATE:-?} blocks/s; peak RSS ${RSS:-?} bytes)"
else
	echo "measure: unchanged (replay status $REPLAY_STATUS; see $ERR)"
fi
exit "$REPLAY_STATUS"
