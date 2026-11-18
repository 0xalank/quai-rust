#!/bin/sh
# Stop a process when free disk space falls below a limit.
#   disk_guard.sh PID DIR MIN_FREE_GB
# It sends SIGINT, waits up to 120 s for a clean shutdown, then sends SIGKILL.
set -eu

pid=$1
dir=$2
min_gb=$3
min_kb=$((min_gb * 1048576))

echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) guarding pid $pid on $dir, limit ${min_gb} GiB"
while kill -0 "$pid" 2>/dev/null; do
	avail_kb=$(df -Pk "$dir" | awk 'NR==2 {print $4}')
	if [ "$avail_kb" -lt "$min_kb" ]; then
		echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) free ${avail_kb} KiB is below ${min_gb} GiB: stopping pid $pid"
		kill -INT "$pid" 2>/dev/null || true
		i=0
		while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 120 ]; do
			sleep 1
			i=$((i + 1))
		done
		if kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null || true; fi
		exit 0
	fi
	sleep 60
done
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) pid $pid exited"
