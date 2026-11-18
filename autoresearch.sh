#!/bin/sh
set -eu
QUAI_RUST_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$QUAI_RUST_ROOT/scripts/tune.py" "$@"
