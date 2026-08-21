#!/bin/sh

set -eu

usage() {
    echo "usage: $0 TARGET" >&2
    exit 2
}

[ "$#" -eq 1 ] || usage

target=$1
if ! LC_ALL=C printf '%s\n' "$target" | grep -Eq '^[A-Za-z0-9_][A-Za-z0-9_.-]*$'; then
    echo "invalid release target: $target" >&2
    exit 1
fi

printf '%s\n' "$target"
