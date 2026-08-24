#!/bin/sh

set -eu

usage() {
    echo "usage: $0 TARGET" >&2
    exit 2
}

[ "$#" -eq 1 ] || usage

target=$1
script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)
targets=$("$script_dir/release-targets.sh")

for supported_target in $targets; do
    if [ "$target" = "$supported_target" ]; then
        printf '%s\n' "$target"
        exit 0
    fi
done

echo "unsupported release target: $target" >&2
echo "supported release targets:" >&2
for supported_target in $targets; do
    printf '  %s\n' "$supported_target" >&2
done
exit 1
