#!/bin/sh
# Run a pinned go-quai mainnet (colosseum) node for slice [0 0].
# The node supplies the blocks for the Rust mainnet replay (ROADMAP.md, Phase 1).
# It is a reference-data source only; nothing in the Rust build depends on it.
#
#   scripts/mainnet/go_node.sh start|stop|status
#
# Environment:
#   QUAI_MAINNET_DIR  data root (default: ~/quai-data/mainnet-v0.57.0)
#   GO_QUAI_BIN       binary to pin on first start (default: ../go-quai/build/bin/go-quai)
#   MIN_FREE_GB       the guard stops the node below this free space (default: 25)
#   ALLOW_METERED     set to 1 to run on an iPhone hotspot (default: refused)
#
# UPnP port mapping and telemetry are off. RPC listens on localhost only.
# scripts/mainnet/node_guard.sh stops the node on low disk or a metered link.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
DATA=${QUAI_MAINNET_DIR:-"$HOME/quai-data/mainnet-v0.57.0"}
SRC_BIN=${GO_QUAI_BIN:-"$ROOT/../go-quai/build/bin/go-quai"}
MIN_FREE_GB=${MIN_FREE_GB:-25}
NODE_PID="$DATA/go-quai.pid"
GUARD_PID="$DATA/disk-guard.pid"

running() { [ -f "$1" ] && kill -0 "$(cat "$1")" 2>/dev/null; }

case "${1:-status}" in
start)
	if running "$NODE_PID"; then
		echo "already running: pid $(cat "$NODE_PID")"
		exit 0
	fi
	mkdir -p "$DATA/bin" "$DATA/db" "$DATA/config" "$DATA/logs"
	# Pin the binary that writes this database. Replace it only with a new data root.
	if [ ! -x "$DATA/bin/go-quai" ]; then
		cp "$SRC_BIN" "$DATA/bin/go-quai"
		shasum -a 256 "$DATA/bin/go-quai" > "$DATA/bin/go-quai.sha256"
		git -C "$(dirname "$SRC_BIN")/../.." rev-parse HEAD > "$DATA/bin/go-quai.commit" 2>/dev/null || true
	fi
	# go-quai reads VERSION and three params/*.json files from its working
	# directory at start (params/version.go, cmd/utils/flags.go). Pin copies.
	if [ ! -f "$DATA/VERSION" ]; then
		SRC_TREE="$(dirname "$SRC_BIN")/../.."
		mkdir -p "$DATA/params"
		cp "$SRC_TREE/VERSION" "$DATA/VERSION"
		for f in genesis_alloc.json forfeiture_addresses.json founder_forfeiture_addresses.json; do
			cp "$SRC_TREE/params/$f" "$DATA/params/$f"
		done
	fi
	free_gb=$(df -Pk "$DATA" | awk 'NR==2 {print int($4 / 1048576)}')
	if [ "$free_gb" -lt "$MIN_FREE_GB" ]; then
		echo "only ${free_gb} GiB free; need more than ${MIN_FREE_GB} GiB" >&2
		exit 1
	fi
	gateway=$(route -n get default 2>/dev/null | awk '/gateway:/ {print $2}')
	case "$gateway" in
	172.20.10.*)
		if [ "${ALLOW_METERED:-0}" != 1 ]; then
			echo "default gateway $gateway is an iPhone hotspot; set ALLOW_METERED=1 to sync over it" >&2
			exit 1
		fi
		;;
	esac
	# go-quai writes nodelogs/ under its working directory.
	cd "$DATA"
	nohup "$DATA/bin/go-quai" start \
		--global.data-dir "$DATA/db" \
		--global.config-dir "$DATA/config" \
		--node.environment colosseum \
		--node.slices "[0 0]" \
		--node.portmap=false \
		--node.telemetry=false \
		>> "$DATA/logs/node.out" 2>&1 < /dev/null &
	echo $! > "$NODE_PID"
	nohup "$ROOT/scripts/mainnet/node_guard.sh" "$(cat "$NODE_PID")" "$DATA" "$MIN_FREE_GB" \
		>> "$DATA/logs/node-guard.log" 2>&1 < /dev/null &
	echo $! > "$GUARD_PID"
	echo "started go-quai pid $(cat "$NODE_PID"); data $DATA; guard stops it below ${MIN_FREE_GB} GiB free or on a hotspot"
	;;
stop)
	if running "$NODE_PID"; then
		kill -INT "$(cat "$NODE_PID")"
		echo "sent SIGINT to go-quai pid $(cat "$NODE_PID")"
	else
		echo "go-quai is not running"
	fi
	if running "$GUARD_PID"; then kill "$(cat "$GUARD_PID")"; fi
	;;
status)
	if running "$NODE_PID"; then echo "go-quai running: pid $(cat "$NODE_PID")"; else echo "go-quai not running"; fi
	if running "$GUARD_PID"; then echo "node guard running: pid $(cat "$GUARD_PID")"; fi
	du -sh "$DATA/db" 2>/dev/null || true
	df -h "$DATA" 2>/dev/null | tail -1
	tail -n 2 "$DATA/logs/node-guard.log" 2>/dev/null || true
	;;
*)
	echo "usage: $0 start|stop|status" >&2
	exit 2
	;;
esac
