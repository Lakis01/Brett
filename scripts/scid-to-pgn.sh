#!/usr/bin/env bash
# Export a SCID database to PGN without ever writing to the source location.
#
# Usage:
#   scripts/scid-to-pgn.sh "/path/to/Database base name" /tmp/opencode/out.pgn
#
# The script copies the .si4/.sn4/.sg4 triple into /tmp/opencode and runs
# scidpgn there. The source directory is only read from.

set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <database-prefix> <output.pgn>" >&2
    exit 2
fi

src_prefix=$1
out=$(realpath -m "$2")
src_dir=$(dirname "$src_prefix")
src_base=$(basename "$src_prefix")

if [ ! -d "$src_dir" ]; then
    echo "error: no such directory: $src_dir" >&2
    exit 1
fi

tmp_dir=$(mktemp -d /tmp/opencode/scid-export.XXXXXX)
trap 'rm -rf "$tmp_dir"' EXIT

for ext in si4 sn4 sg4; do
    if [ ! -f "$src_dir/$src_base.$ext" ]; then
        echo "error: missing $src_dir/$src_base.$ext" >&2
        exit 1
    fi
    cp -p "$src_dir/$src_base.$ext" "$tmp_dir/"
done

mkdir -p "$(dirname "$out")"
(
    cd "$tmp_dir"
    scidpgn "$src_base"
) > "$out"

printf 'wrote %s (%s games)\n' "$out" "$(grep -c '^\[Event ' "$out" || true)"
