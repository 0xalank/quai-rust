#!/bin/sh
# Rolling mainnet replay (ROADMAP.md, Phase 1): replay to the live head with
# bounded disk. The peer export (QEX1) stays at most ROLLING_AHEAD zone heights
# ahead of the latest replay checkpoint, and segment files are deleted after
# the replay has checkpointed past them. Disk use stays near the replay data
# directory plus a few segments (a segment is about 650 MB).
#
#   scripts/mainnet/rolling_replay.sh [--once]
#
# Each pass:
#   1. guards: a STOP file, free disk, a metered gateway (no download);
#   2. prune: peer-export prune --below <oldest kept checkpoint>, so that a
#      fallback to the older checkpoint still finds its segment;
#   3. export: peer-export --sources public --to <latest checkpoint + AHEAD>;
#   4. replay: quai-node replay --state --resume, to the end of the export;
#   5. prune again; append one JSON line to the history file.
# When the replay has reached the export and the peers have nothing new, the
# script waits ROLLING_POLL seconds and polls again: it follows the head.
# --once runs one pass.
#
# Stops (exit status): ROLLING_TO reached or a STOP file in ROLLING_DIR (0;
# a running replay stops at its next checkpoint, a running export at its next
# check); the first replay failure, printed (1, or 2 for a setup error);
# another run holds the lock, or another quai-node or peer-export uses the
# export or the data directory (3); free disk below MIN_FREE_GB (4);
# EXPORT_RETRIES failed exports in a row (5).
#
# Environment:
#   QUAI_MAINNET_DIR  data root (default: ~/quai-data/mainnet-v0.57.0)
#   ROLLING_DIR       lock, STOP file, logs, pinned binaries
#                     (default: $QUAI_MAINNET_DIR/rolling)
#   ROLLING_EXPORT    the peer export (default: $ROLLING_DIR/export)
#   ROLLING_DATADIR   quai-node data directory (default: $ROLLING_DIR/quai-node)
#   ROLLING_AHEAD     zone heights the export may run ahead of the latest
#                     checkpoint (default: 100000, one segment)
#   ROLLING_POLL      seconds between polls at the head (default: 600)
#   ROLLING_TO        stop after the replay passes this zone height
#                     (default: none, follow the head)
#   ROLLING_HISTORY   history file (default: results/rolling-replay/history.jsonl)
#   EXPORT_RETRIES    failed exports in a row before the script stops (default: 3)
#   EXPORT_ARGS       more peer-export options, for example "--max-peers 6"
#   REPLAY_JOBS       proof-of-work threads (default: 3)
#   MIN_FREE_GB       stop below this free space (default: 30)
#   ALLOW_METERED     1 allows downloads on an iPhone hotspot (172.20.10.x)
#   NICE              nice level of the export and the replay (default: 10)
#   QUAI_NODE_BIN, PEER_EXPORT_BIN
#                     binaries to use. Without them the script builds both
#                     (CARGO_BUILD_JOBS, default 3) and pins copies in
#                     $ROLLING_DIR/bin for the whole run.
set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
DATA=${QUAI_MAINNET_DIR:-"$HOME/quai-data/mainnet-v0.57.0"}
WORK=${ROLLING_DIR:-"$DATA/rolling"}
EXPORT=${ROLLING_EXPORT:-"$WORK/export"}
DATADIR=${ROLLING_DATADIR:-"$WORK/quai-node"}
AHEAD=${ROLLING_AHEAD:-100000}
POLL=${ROLLING_POLL:-600}
TO=${ROLLING_TO:-}
HISTORY=${ROLLING_HISTORY:-"$ROOT/results/rolling-replay/history.jsonl"}
RETRIES=${EXPORT_RETRIES:-3}
JOBS=${REPLAY_JOBS:-3}
MIN_FREE_GB=${MIN_FREE_GB:-30}
NICE=${NICE:-10}
# Checkpoints of a --state replay of mainnet (quai-node store::chain_dir):
# <datadir>/replay/<environment>-<first 4 genesis bytes>/state.
STATE="$DATADIR/replay/colosseum-ac81c28f/state"
LOCK="$WORK/lock"
STOP="$WORK/STOP"
LOGS="$WORK/logs"
ONCE=0

case "${1:-}" in
"") ;;
--once) ONCE=1 ;;
*) sed -n '8p' "$0" | sed 's/^# *//' >&2; exit 2 ;;
esac

log() { echo "rolling_replay.sh: $(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >&2; }
is_number() { case "$1" in "" | *[!0-9]*) return 1 ;; *) return 0 ;; esac; }
# Another quai-node or peer-export process on the same directories, for
# example the long replay that this pipeline continues. A prune or an
# export would change the export under it.
in_use() {
	ps -A -o pid=,command= 2>/dev/null | grep -F -e "$DATADIR" -e "$EXPORT" |
		grep -e quai-node -e peer-export | grep -v -e grep -e rolling_replay
}
busy=$(in_use)
if [ -n "$busy" ]; then
	log "the export or the data directory is in use; stop this first: $busy"
	exit 3
fi
for v in "$AHEAD" "$POLL" "$RETRIES" "$JOBS" "$MIN_FREE_GB" ${TO:+"$TO"}; do
	is_number "$v" || { log "not a number: $v"; exit 2; }
done
[ "$AHEAD" -gt 0 ] || { log "ROLLING_AHEAD must be positive"; exit 2; }

# Lock: mkdir is atomic. A lock whose process is gone is stale.
mkdir -p "$WORK" "$LOGS" "$(dirname -- "$HISTORY")"
if ! mkdir "$LOCK" 2>/dev/null; then
	other=$(cat "$LOCK/pid" 2>/dev/null || true)
	if [ -n "$other" ] && kill -0 "$other" 2>/dev/null; then
		log "another run is active (pid $other)"
		exit 3
	fi
	log "removing the stale lock of pid ${other:-unknown}"
	rm -rf "$LOCK"
	mkdir "$LOCK" || { log "cannot take the lock $LOCK"; exit 3; }
fi
echo $$ > "$LOCK/pid"
CHILD=
trap 'rm -rf "$LOCK"' EXIT
trap '[ -n "$CHILD" ] && kill -TERM "$CHILD" 2>/dev/null; exit 130' INT TERM
if [ -e "$STOP" ]; then
	log "removing an old STOP file"
	rm -f "$STOP"
fi

# Binaries: pinned copies, so a rebuild elsewhere does not change a long run.
QUAI_NODE=${QUAI_NODE_BIN:-}
PEER_EXPORT=${PEER_EXPORT_BIN:-}
if [ -z "$QUAI_NODE" ] || [ -z "$PEER_EXPORT" ]; then
	log "building quai-node and peer-export"
	(cd "$ROOT" && export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3} &&
		nice -n "$NICE" cargo build --release --locked -p quai-node &&
		nice -n "$NICE" cargo build --release --locked -p quai-network \
			--features native-transport --example peer-export) >&2 ||
		{ log "build failed"; exit 2; }
	TARGET=${CARGO_TARGET_DIR:-"$ROOT/target"}
	REV=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
	git -C "$ROOT" diff --quiet HEAD -- 2>/dev/null || REV="$REV-dirty"
	mkdir -p "$WORK/bin"
	for b in quai-node:"$TARGET/release/quai-node" peer-export:"$TARGET/release/examples/peer-export"; do
		name=${b%%:*}
		cp "${b#*:}" "$WORK/bin/.$name.tmp" && mv -f "$WORK/bin/.$name.tmp" "$WORK/bin/$name-$REV" ||
			{ log "cannot pin $name"; exit 2; }
	done
	[ -n "$QUAI_NODE" ] || QUAI_NODE="$WORK/bin/quai-node-$REV"
	[ -n "$PEER_EXPORT" ] || PEER_EXPORT="$WORK/bin/peer-export-$REV"
fi
for b in "$QUAI_NODE" "$PEER_EXPORT"; do
	[ -x "$b" ] || { log "not an executable: $b"; exit 2; }
	log "binary $b sha256 $(shasum -a 256 "$b" | cut -c1-16)"
done

# Checkpoint heights of the replay, ascending.
checkpoints() {
	ls "$STATE" 2>/dev/null | sed -n 's/^checkpoint-\([0-9]\{10\}\)\.qrc$/\1/p' |
		sed 's/^0*\([0-9]\)/\1/' | sort -n
}
latest_checkpoint() { checkpoints | tail -n 1; }
oldest_checkpoint() { checkpoints | head -n 1; }
# One field of the one-line JSON events that peer-export prints.
json_field() { sed -n "s/.*\"$1\":\([^,}]*\).*/\1/p" | tail -n 1 | tr -d '"'; }
export_status() {
	[ -f "$EXPORT/manifest.json" ] || return 0
	"$PEER_EXPORT" status "$EXPORT" 2>/dev/null | tail -n 1
}
free_gb() {
	m=
	for d in "$WORK" "$(dirname -- "$EXPORT")" "$(dirname -- "$DATADIR")"; do
		f=$(df -Pk "$d" 2>/dev/null | awk 'NR==2 {print int($4 / 1048576)}')
		is_number "$f" || continue
		if [ -z "$m" ] || [ "$f" -lt "$m" ]; then m=$f; fi
	done
	echo "${m:-0}"
}
metered() {
	[ "${ALLOW_METERED:-0}" = 1 ] && return 1
	gateway=$(route -n get default 2>/dev/null | awk '/gateway:/ {print $2}')
	case "$gateway" in
	172.20.10.*) return 0 ;;
	*) return 1 ;;
	esac
}
# Delete the segment files that no kept checkpoint needs. Nothing is pruned
# before the replay has a checkpoint.
prune_export() {
	floor=$(oldest_checkpoint)
	[ -n "$floor" ] && [ -f "$EXPORT/manifest.json" ] || return 0
	out=$("$PEER_EXPORT" prune "$EXPORT" --below "$floor" 2>&1)
	status=$?
	line=$(printf '%s\n' "$out" | tail -n 1)
	if [ "$status" != 0 ]; then
		log "prune failed (status $status): $line"
		return 1
	fi
	echo "$line" >> "$LOGS/prune.jsonl"
	freed=$(echo "$line" | json_field freed_bytes)
	if is_number "$freed" && [ "$freed" -gt 0 ]; then
		log "pruned below checkpoint $floor: $freed bytes freed, $(echo "$line" | json_field pruned_segments) segments pruned in all"
	fi
	PRUNED=$(echo "$line" | json_field pruned_segments)
}

# Run a command in the background and watch it. A STOP file asks the export
# to stop at its next check and the replay to stop at its next checkpoint.
# Low disk stops the replay at once and the export at its next check (the
# export also checks the disk itself). Returns the command's exit status.
INTERRUPTED=
run_watched() {
	kind=$1 out=$2 err=$3
	shift 3
	nice -n "$NICE" "$@" > "$out" 2> "$err" &
	CHILD=$!
	stop_at=
	asked=
	while kill -0 "$CHILD" 2>/dev/null; do
		sleep 10
		if [ -e "$STOP" ] && [ -z "$asked" ]; then
			asked=1
			INTERRUPTED=STOP
			if [ "$kind" = export ]; then
				log "STOP: the export stops at its next check"
				touch "$EXPORT/STOP"
			else
				stop_at=$(latest_checkpoint)
				log "STOP: the replay stops at its next checkpoint (after ${stop_at:-none})"
			fi
		fi
		if [ -n "$asked" ] && [ "$kind" = replay ] && [ "$(latest_checkpoint)" != "$stop_at" ]; then
			kill -TERM "$CHILD" 2>/dev/null
		fi
		free=$(free_gb)
		if [ "$free" -lt "$MIN_FREE_GB" ] && [ "$INTERRUPTED" != disk ]; then
			log "only $free GiB free (limit $MIN_FREE_GB): stopping the $kind"
			INTERRUPTED=disk
			if [ "$kind" = export ]; then
				touch "$EXPORT/STOP"
			else
				kill -TERM "$CHILD" 2>/dev/null
			fi
		fi
	done
	wait "$CHILD"
	status=$?
	CHILD=
	return "$status"
}

log "export $EXPORT; replay data $DATADIR; ahead $AHEAD; poll ${POLL}s; to ${TO:-head}; jobs $JOBS; min free ${MIN_FREE_GB} GiB"
FAILS=0
PASS=0
EXIT=0
while :; do
	PASS=$((PASS + 1))
	STAMP=$(date -u +%Y%m%dT%H%M%SZ)
	START_UTC=$(date -u +%Y-%m-%dT%H:%M:%SZ)
	if [ -e "$STOP" ]; then log "STOP file: stopping"; break; fi
	busy=$(in_use)
	if [ -n "$busy" ]; then
		log "the export or the data directory is in use; stopping: $busy"
		EXIT=3
		break
	fi
	free=$(free_gb)
	if [ "$free" -lt "$MIN_FREE_GB" ]; then
		log "only $free GiB free (limit $MIN_FREE_GB): stopping"
		EXIT=4
		break
	fi
	BEFORE=$(latest_checkpoint)
	if [ -n "$TO" ] && [ -n "$BEFORE" ] && [ "$BEFORE" -ge "$TO" ]; then
		log "the replay reached checkpoint $BEFORE (ROLLING_TO $TO): done"
		break
	fi
	PRUNED=
	prune_export

	# Export: at most AHEAD zone heights past the latest checkpoint.
	LAST=$(export_status | json_field last_zone)
	TARGET=$((${BEFORE:-0} + AHEAD))
	if [ -n "$TO" ] && [ "$TARGET" -gt "$TO" ]; then TARGET=$TO; fi
	EXPORT_RESULT=ahead
	if is_number "$LAST" && [ "$LAST" -ge "$TARGET" ]; then
		:
	elif metered; then
		EXPORT_RESULT=metered
		log "the default gateway is an iPhone hotspot: no download (ALLOW_METERED=1 allows it)"
	else
		log "pass $PASS: export to zone $TARGET (export ends at ${LAST:-none})"
		# Word splitting of EXPORT_ARGS is intended.
		# shellcheck disable=SC2086
		run_watched export "$LOGS/$STAMP-export.jsonl" "$LOGS/$STAMP-export.err" \
			"$PEER_EXPORT" export --out "$EXPORT" --sources public --to "$TARGET" \
			--min-free-gb "$MIN_FREE_GB" ${EXPORT_ARGS:-}
		status=$?
		if [ "$status" = 0 ]; then
			FAILS=0
			EXPORT_RESULT=$(tail -n 1 "$LOGS/$STAMP-export.jsonl" | json_field event)
		else
			FAILS=$((FAILS + 1))
			EXPORT_RESULT=failed
			log "export failed with status $status ($FAILS in a row): $(tail -n 1 "$LOGS/$STAMP-export.err")"
		fi
		LAST=$(export_status | json_field last_zone)
	fi

	# Replay: from the latest checkpoint to the end of the export.
	REPLAY_STATUS=null
	RESULT=
	if is_number "$LAST" && { [ -z "$BEFORE" ] || [ "$LAST" -gt "$BEFORE" ]; } && [ -z "$INTERRUPTED" ]; then
		OUT="$LOGS/$STAMP-replay.out"
		ERR="$LOGS/$STAMP-replay.log"
		log "pass $PASS: replay from checkpoint ${BEFORE:-genesis} to zone $LAST${TO:+ (stop after $TO)} (log: $ERR)"
		set -- --datadir "$DATADIR" replay --export "$EXPORT" --state --resume --jobs "$JOBS" --progress 300
		if [ -n "$TO" ]; then set -- "$@" --to "$TO"; fi
		: > "$LOGS/replay.start"
		run_watched replay "$OUT" "$ERR" "$QUAI_NODE" "$@"
		REPLAY_STATUS=$?
		RESULT=$(grep '^replay ' "$OUT" | tail -n 1)
	fi
	prune_export
	AFTER=$(latest_checkpoint)

	# The replay writes its report when it ends; a stopped replay leaves the
	# report of an earlier run, which must not count.
	REPORT="$DATADIR/replay/last.json"
	field() {
		[ "$REPLAY_STATUS" != null ] && [ "$REPORT" -nt "$LOGS/replay.start" ] &&
			sed -n "s/^ *\"$1\": \\([0-9.a-z]*\\),*$/\\1/p" "$REPORT" 2>/dev/null | head -n 1
	}
	HIGHEST=$(field highest_passed_zone_height)
	RUN_HEIGHTS=$(field heights_this_run)
	RATE=$(field zone_blocks_per_second)
	KEPT=$(export_status | json_field kept_bytes)
	DATADIR_KB=$(du -sk "$DATADIR" 2>/dev/null | awk '{print $1}')
	escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
	printf '{"utc":"%s","pass":%s,"checkpoint_before":%s,"export_target":%s,"export":"%s","export_last_zone":%s,"replay_status":%s,"highest_passed_zone_height":%s,"checkpoint_after":%s,"heights_this_run":%s,"zone_blocks_per_second":%s,"pruned_segments":%s,"export_kept_bytes":%s,"datadir_bytes":%s,"free_gb":%s,"result":"%s"}\n' \
		"$START_UTC" "$PASS" "${BEFORE:-null}" "$TARGET" "$EXPORT_RESULT" "${LAST:-null}" \
		"$REPLAY_STATUS" "${HIGHEST:-null}" "${AFTER:-null}" "${RUN_HEIGHTS:-null}" "${RATE:-null}" \
		"${PRUNED:-null}" "${KEPT:-null}" "$(( ${DATADIR_KB:-0} * 1024 ))" "$(free_gb)" \
		"$(escape "$RESULT")" >> "$HISTORY"

	if [ "$REPLAY_STATUS" != null ]; then
		echo "$RESULT"
		if [ -n "$INTERRUPTED" ] && [ "$REPLAY_STATUS" -gt 2 ]; then
			log "the replay was stopped ($INTERRUPTED) after checkpoint ${AFTER:-none}"
		elif [ "$REPLAY_STATUS" != 0 ]; then
			log "replay failed (status $REPLAY_STATUS): ${RESULT:-see $ERR}"
			EXIT=$REPLAY_STATUS
			break
		else
			echo "measure: mainnet zone ${HIGHEST:-?} replayed with every root equal to go-quai v0.57.0 (export ends at ${LAST:-?}; ${RATE:-?} blocks/s)"
		fi
	fi
	if [ "$INTERRUPTED" = disk ]; then EXIT=4; break; fi
	if [ "$INTERRUPTED" = STOP ]; then break; fi
	if [ "$FAILS" -ge "$RETRIES" ]; then
		log "stopping after $FAILS failed exports in a row"
		EXIT=5
		break
	fi
	if [ "$ONCE" = 1 ]; then break; fi
	if [ "$REPLAY_STATUS" = null ]; then
		# Nothing new to replay: at the peers' head, metered or after a failed
		# export. Poll again later; a STOP file ends the wait.
		log "nothing new to replay (export: $EXPORT_RESULT); next poll in ${POLL}s"
		waited=0
		while [ "$waited" -lt "$POLL" ] && [ ! -e "$STOP" ]; do
			sleep 10
			waited=$((waited + 10))
		done
	fi
done
exit "$EXIT"
