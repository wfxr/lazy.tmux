#!/bin/sh

set -eu

usage() {
    echo "usage: $0 TAG [MANIFEST_PATH]" >&2
    exit 2
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage

tag=$1
script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)
manifest_path=${2:-"$script_dir/../../Cargo.toml"}

semver_pattern='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-((0|[1-9][0-9]*)|[0-9]*[A-Za-z-][0-9A-Za-z-]*)(\.((0|[1-9][0-9]*)|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?$'

if ! LC_ALL=C printf '%s\n' "$tag" | grep -Eq "$semver_pattern"; then
    echo "release tag must be v-prefixed SemVer without build metadata: $tag" >&2
    exit 1
fi

if [ ! -f "$manifest_path" ]; then
    echo "Cargo manifest not found: $manifest_path" >&2
    exit 1
fi

package_version=$(
    awk '
        /^\[package\]$/ { in_package = 1; next }
        in_package && /^\[/ { exit }
        in_package && /^[[:space:]]*version[[:space:]]*=/ {
            line = $0
            sub(/^[^=]*=[[:space:]]*"/, "", line)
            sub(/".*$/, "", line)
            print line
            exit
        }
    ' "$manifest_path"
)

if [ -z "$package_version" ]; then
    echo "Cargo package version not found in $manifest_path" >&2
    exit 1
fi

version=${tag#v}
if [ "$version" != "$package_version" ]; then
    echo "release tag $tag does not match Cargo package version v$package_version" >&2
    exit 1
fi

printf '%s\n' "$version"
