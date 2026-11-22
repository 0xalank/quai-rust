#!/bin/sh
# Shadow run (ROADMAP.md, Phase 2 step 4): a long stateless `quai-node run`
# that follows the mainnet head and counts every divergence from the go-quai
# peers (a block or workshare they accept and the node rejects, the
# canonical chain a few blocks below the head differing from theirs, the head
# left behind theirs).
#
# TEST TOOL, NOT A PRODUCTION NODE: the run starts from a shadow bootstrap
# (`--shadow-bootstrap`), which trusts one zone block and its ancestry
# without validating them. Every block after that block passes every
# stateless check. A `quai-node run` without the flag refuses this data
# directory.
#
#   scripts/mainnet/shadow_run.sh [--once]
#
# The script starts the node, watches it and starts it again after a crash
# (the node resumes from its last checkpoint; the bootstrap runs only while
# the data directory holds no checkpoint). --once does not restart.
#
# Stops (exit status): SHADOW_DURATION reached or a STOP file in SHADOW_DIR
# (0; the node stops after its current block and writes a checkpoint); a
# block of the peers' canonical chain failed a check in the sync by number
# (1: a divergence; the node cannot go past it; a "peer data" failure
# restarts instead); a setup error (2); another
# run holds the lock or uses the data directory (3); free disk below
# MIN_FREE_GB (4); MAX_RESTARTS restarts within an hour (5); the default
# gateway is an iPhone hotspot, 172.20.10.x (6).
#
# Environment:
#   QUAI_MAINNET_DIR  data root (default: ~/quai-data/mainnet-v0.57.0)
#   SHADOW_DIR        lock, STOP file, logs, history, pinned binary
#                     (default: $QUAI_MAINNET_DIR/shadow)
#   SHADOW_DATADIR    quai-node data directory (default: $SHADOW_DIR/quai-node)
#   SHADOW_BOOTSTRAP  the trusted zone block hash, or auto: the node chooses
#                     the block 1,000 below the best peer head that at least
#                     3 peers report as canonical (default: auto). Used only
#                     while the data directory holds no checkpoint.
#   SHADOW_DURATION   seconds to run in all, restarts included
#                     (default: 1209600, two weeks; 0: no limit)
#   METRICS_LISTEN    Prometheus address (default: 127.0.0.1:9102)
#   SHADOW_STORE      1 keeps the block store (about 50 KB per zone block,
#                     about 12 GB in two weeks) and allows RPC_LISTEN;
#                     default 0: no store, bounded disk
#   RPC_LISTEN        RPC address (default: off; needs SHADOW_STORE=1)
#   RUN_JOBS          proof-of-work threads (default: 3)
#   MIN_FREE_GB       stop below this free space (default: 30)
#   ALLOW_METERED     1 allows an iPhone hotspot (172.20.10.x)
#   NICE              nice level of the node (default: 10)
#   RESTART_DELAY     seconds before a restart (default: 30)
#   STALL_SECONDS     restart the node when its head has not moved for this
#                     long while the peers are more than STALL_GAP blocks
#                     ahead (default: 900)
#   STALL_GAP         see STALL_SECONDS (default: 10)
#   MAX_RESTARTS      restarts within an hour before the script stops (default: 6)
#   QUAI_NODE_BIN     binary to use. Without it the script builds quai-node
#                     (CARGO_BUILD_JOBS, default 3) and pins a copy in
#                     $SHADOW_DIR/bin for the whole run.
set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
DATA=${QUAI_MAINNET_DIR:-"$HOME/quai-data/mainnet-v0.57.0"}
WORK=${SHADOW_DIR:-"$DATA/shadow"}
DATADIR=${SHADOW_DATADIR:-"$WORK/quai-node"}
BOOT=${SHADOW_BOOTSTRAP:-auto}
DURATION=${SHADOW_DURATION:-1209600}
METRICS=${METRICS_LISTEN:-127.0.0.1:9102}
STORE=${SHADOW_STORE:-0}
RPC=${RPC_LISTEN:-off}
JOBS=${RUN_JOBS:-3}
MIN_FREE_GB=${MIN_FREE_GB:-30}
NICE=${NICE:-10}
DELAY=${RESTART_DELAY:-30}
MAX_RESTARTS=${MAX_RESTARTS:-6}
STALL_SECONDS=${STALL_SECONDS:-900}
STALL_GAP=${STALL_GAP:-10}
LOCK="$WORK/lock"
STOP="$WORK/STOP"
LOGS="$WORK/logs"
HISTORY="$WORK/history.jsonl"
ONCE=0

case "${1:-}" in
"") ;;
--once) ONCE=1 ;;
*) sed -n '15p' "$0" | sed 's/^# *//' >&2; exit 2 ;;
esac

log() { echo "shadow_run.sh: $(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >&2; }
is_number() { case "$1" in "" | *[!0-9]*) return 1 ;; *) return 0 ;; esac; }
for v in "$DURATION" "$JOBS" "$MIN_FREE_GB" "$DELAY" "$MAX_RESTARTS" "$STALL_SECONDS" "$STALL_GAP"; do
	is_number "$v" || { log "not a number: $v"; exit 2; }
done
case "$BOOT" in
auto | 0x????????????????????????????????????????????????????????????????) ;;
*) log "SHADOW_BOOTSTRAP must be auto or a 0x-prefixed 32-byte hash"; exit 2 ;;
esac
case "$STORE" in
1) STORE_ARG= ;;
0)
	STORE_ARG=--no-store
	[ "$RPC" = off ] || { log "RPC_LISTEN needs SHADOW_STORE=1"; exit 2; }
	;;
*) log "SHADOW_STORE must be 0 or 1"; exit 2 ;;
esac
in_use() {
	ps -A -o pid=,command= 2>/dev/null | grep -F -e "$DATADIR" | grep quai-node |
		grep -v -e grep -e shadow_run
}
metered() {
	[ "${ALLOW_METERED:-0}" = 1 ] && return 1
	gateway=$(route -n get default 2>/dev/null | awk '/gateway:/ {print $2}')
	case "$gateway" in
	172.20.10.*) return 0 ;;
	*) return 1 ;;
	esac
}
free_gb() {
	m=
	for d in "$WORK" "$(dirname -- "$DATADIR")"; do
		f=$(df -Pk "$d" 2>/dev/null | awk 'NR==2 {print int($4 / 1048576)}')
		is_number "$f" || continue
		if [ -z "$m" ] || [ "$f" -lt "$m" ]; then m=$f; fi
	done
	echo "${m:-0}"
}

busy=$(in_use)
if [ -n "$busy" ]; then
	log "the data directory is in use; stop this first: $busy"
	exit 3
fi
if metered; then
	log "the default gateway is an iPhone hotspot: no run (ALLOW_METERED=1 allows it)"
	exit 6
fi
mkdir -p "$WORK" "$LOGS" "$DATADIR"
if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
	log "only $(free_gb) GiB free (limit $MIN_FREE_GB): no run"
	exit 4
fi
# Lock: mkdir is atomic. A lock whose process is gone is stale.
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
trap '[ -n "$CHILD" ] && kill -TERM "$CHILD" 2>/dev/null && wait "$CHILD"; exit 130' INT TERM
if [ -e "$STOP" ]; then
	log "removing an old STOP file"
	rm -f "$STOP"
fi

# Binary: a pinned copy, so a rebuild elsewhere does not change a long run.
QUAI_NODE=${QUAI_NODE_BIN:-}
if [ -z "$QUAI_NODE" ]; then
	log "building quai-node"
	(cd "$ROOT" && export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3} &&
		nice -n "$NICE" cargo build --release --locked -p quai-node) >&2 ||
		{ log "build failed"; exit 2; }
	TARGET=${CARGO_TARGET_DIR:-"$ROOT/target"}
	REV=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
	git -C "$ROOT" diff --quiet HEAD -- 2>/dev/null || REV="$REV-dirty"
	mkdir -p "$WORK/bin"
	cp "$TARGET/release/quai-node" "$WORK/bin/.quai-node.tmp" &&
		mv -f "$WORK/bin/.quai-node.tmp" "$WORK/bin/quai-node-$REV" ||
		{ log "cannot pin quai-node"; exit 2; }
	QUAI_NODE="$WORK/bin/quai-node-$REV"
fi
[ -x "$QUAI_NODE" ] || { log "not an executable: $QUAI_NODE"; exit 2; }
SHA=$(shasum -a 256 "$QUAI_NODE" | cut -c1-16)
log "binary $QUAI_NODE sha256 $SHA"
log "TEST TOOL: shadow run from a trusted bootstrap block (--shadow-bootstrap $BOOT); the bootstrap block and its ancestry are not validated by the node"
log "data $DATADIR; metrics $METRICS; rpc $RPC; block store $STORE; jobs $JOBS; duration ${DURATION}s; min free ${MIN_FREE_GB} GiB"

escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
metric() { curl -s --max-time 5 "http://$METRICS/metrics" 2>/dev/null | awk -v n="$1" '$1 == n {print $2}'; }
STARTED=$(date +%s)
RESTARTS=""
EXIT=0
while :; do
	if [ -e "$STOP" ]; then log "STOP file: stopping"; break; fi
	if metered; then log "the default gateway is an iPhone hotspot: stopping"; EXIT=6; break; fi
	if [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
		log "only $(free_gb) GiB free (limit $MIN_FREE_GB): stopping"
		EXIT=4
		break
	fi
	NOW=$(date +%s)
	if [ "$DURATION" -gt 0 ] && [ $((NOW - STARTED)) -ge "$DURATION" ]; then
		log "the run lasted ${DURATION}s: done"
		break
	fi
	STAMP=$(date -u +%Y%m%dT%H%M%SZ)
	OUT="$LOGS/$STAMP-run.out"
	ERR="$LOGS/$STAMP-run.log"
	log "starting the node (log: $ERR)"
	# Word splitting of STORE_ARG is intended (empty or --no-store).
	# shellcheck disable=SC2086
	nice -n "$NICE" "$QUAI_NODE" --datadir "$DATADIR" --log-level info run --stateless \
		--shadow-bootstrap "$BOOT" --metrics-listen "$METRICS" --rpc-listen "$RPC" \
		$STORE_ARG --jobs "$JOBS" > "$OUT" 2> "$ERR" &
	CHILD=$!
	REASON=
	LAST_HEAD=
	LAST_MOVE=$(date +%s)
	while kill -0 "$CHILD" 2>/dev/null; do
		sleep 10
		if [ -z "$REASON" ]; then
			# Stall watchdog: the head has not moved for STALL_SECONDS while the
			# peers report a head more than STALL_GAP blocks higher (seen after a
			# network change or a sleep: gossip intake stopped and the peers'
			# answers stopped too, so the node's own stall check saw nothing).
			NOW_HEAD=$(metric quai_node_head_height)
			PEER_HEAD=$(metric quai_node_peer_head_height)
			if is_number "$NOW_HEAD" && [ "$NOW_HEAD" != "$LAST_HEAD" ]; then
				LAST_HEAD=$NOW_HEAD
				LAST_MOVE=$(date +%s)
			fi
			if [ -e "$STOP" ]; then
				REASON=STOP
			elif is_number "$NOW_HEAD" && is_number "$PEER_HEAD" &&
				[ "$PEER_HEAD" -gt $((NOW_HEAD + STALL_GAP)) ] &&
				[ $(($(date +%s) - LAST_MOVE)) -ge "$STALL_SECONDS" ]; then
				REASON=stall
				log "stall: head $NOW_HEAD unchanged for $(($(date +%s) - LAST_MOVE))s while the peers report $PEER_HEAD; starting the node again"
			elif metered; then
				REASON=metered
			elif [ "$(free_gb)" -lt "$MIN_FREE_GB" ]; then
				REASON=disk
			elif [ "$DURATION" -gt 0 ] && [ $(($(date +%s) - STARTED)) -ge "$DURATION" ]; then
				REASON=duration
			fi
			if [ -n "$REASON" ]; then
				log "stopping the node ($REASON)"
				kill -TERM "$CHILD" 2>/dev/null
			fi
		fi
	done
	wait "$CHILD"
	STATUS=$?
	CHILD=
	RESULT=$(grep '^run ' "$OUT" | tail -n 1)
	HEAD=$(metric quai_node_head_height)
	printf '{"utc":"%s","start":"%s","status":%s,"reason":"%s","binary_sha256":"%s","result":"%s","last_error":"%s"}\n' \
		"$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$STAMP" "$STATUS" "${REASON:-exit}" "$SHA" \
		"$(escape "$RESULT")" "$(escape "$(grep -e '^ERROR' -e DIVERGENCE "$ERR" | tail -n 1)")" >> "$HISTORY"
	log "the node ended with status $STATUS (${REASON:-exit}): ${RESULT:-see $ERR}${HEAD:+; head $HEAD}"
	case "$REASON" in
	STOP | duration) break ;;
	metered) EXIT=6; break ;;
	disk) EXIT=4; break ;;
	esac
	if [ "$STATUS" = 1 ]; then
		case "$RESULT" in
		*'check="peer data"'*)
			# The peers gave no valid reply (for example a batch past their
			# head). That is a network condition, not a divergence: start
			# again from the checkpoint, within the MAX_RESTARTS limit.
			log "peer data failure, not a divergence; starting again: $RESULT"
			;;
		*)
			log "a canonical block failed a check (a divergence): ${RESULT:-see $ERR}"
			EXIT=1
			break
			;;
		esac
	fi
	if [ "$ONCE" = 1 ]; then EXIT=$STATUS; break; fi
	# A crash: start again after a delay, unless it keeps crashing.
	NOW=$(date +%s)
	RECENT=
	for t in $RESTARTS; do
		if [ $((NOW - t)) -lt 3600 ]; then RECENT="$RECENT $t"; fi
	done
	RESTARTS="$RECENT $NOW"
	COUNT=$(echo $RESTARTS | wc -w | tr -d ' ')
	if [ "$COUNT" -gt "$MAX_RESTARTS" ]; then
		log "stopping after $COUNT restarts within an hour"
		EXIT=5
		break
	fi
	log "restart $COUNT within an hour in ${DELAY}s"
	waited=0
	while [ "$waited" -lt "$DELAY" ] && [ ! -e "$STOP" ]; do
		sleep 5
		waited=$((waited + 5))
	done
done
exit "$EXIT"
