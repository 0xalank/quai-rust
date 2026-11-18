#!/bin/sh
# Stop the mainnet node when it must not run:
#   - free disk space falls below MIN_FREE_GB, or
#   - the default gateway is an iPhone Personal Hotspot (172.20.10.x), where a
#     mainnet sync would use cellular data. Set ALLOW_METERED=1 to permit it.
#
#   node_guard.sh PID DIR MIN_FREE_GB
#
# It sends SIGINT, waits up to 120 s for a clean shutdown, then sends SIGKILL.
# It does not restart the node; run scripts/mainnet/go_node.sh start again.
set -eu

pid=$1
dir=$2
min_gb=$3
min_kb=$((min_gb * 1048576))

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }

metered() {
	[ "${ALLOW_METERED:-0}" = 1 ] && return 1
	gateway=$(route -n get default 2>/dev/null | awk '/gateway:/ {print $2}')
	case "$gateway" in
	172.20.10.*) return 0 ;;
	*) return 1 ;;
	esac
}

stop_node() {
	echo "$(now) $1: stopping pid $pid"
	kill -INT "$pid" 2>/dev/null || true
	i=0
	while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 120 ]; do
		sleep 1
		i=$((i + 1))
	done
	if kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null || true; fi
	exit 0
}

echo "$(now) guarding pid $pid on $dir: disk limit ${min_gb} GiB, metered networks $([ "${ALLOW_METERED:-0}" = 1 ] && echo allowed || echo refused)"
while kill -0 "$pid" 2>/dev/null; do
	avail_kb=$(df -Pk "$dir" | awk 'NR==2 {print $4}')
	if [ "$avail_kb" -lt "$min_kb" ]; then
		stop_node "free ${avail_kb} KiB is below ${min_gb} GiB"
	fi
	if metered; then
		stop_node "default gateway is an iPhone hotspot (metered)"
	fi
	sleep 60
done
echo "$(now) pid $pid exited"
