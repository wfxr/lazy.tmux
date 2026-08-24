#!/bin/sh

set -eu

usage() {
    echo "usage: $0 [--structure-only] TAG TARGET ARCHIVE [MANIFEST_PATH]" >&2
    exit 2
}

fail() {
    echo "invalid release archive: $*" >&2
    exit 1
}

run_smoke_test=true
if [ "${1:-}" = --structure-only ]; then
    run_smoke_test=false
    shift
fi

[ "$#" -ge 3 ] && [ "$#" -le 4 ] || usage

tag=$1
target=$2
archive=$3
manifest_path=${4:-}
script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)

if [ -n "$manifest_path" ]; then
    version=$("$script_dir/validate-version.sh" "$tag" "$manifest_path")
else
    version=$("$script_dir/validate-version.sh" "$tag")
fi
target=$("$script_dir/validate-target.sh" "$target")

package_name="tmup-v${version}-${target}"
expected_archive_name="${package_name}.tar.gz"

[ -f "$archive" ] || fail "file not found: $archive"
[ "$(basename "$archive")" = "$expected_archive_name" ] ||
    fail "expected filename $expected_archive_name"

if ! listing=$(tar -tzf "$archive"); then
    fail "cannot list $archive"
fi
expected_listing=$(printf '%s\n%s\n' "$package_name/" "$package_name/tmup")
[ "$listing" = "$expected_listing" ] || fail "expected only $package_name/tmup"

temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/tmup-archive.XXXXXX")
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

if ! tar -xzf "$archive" -C "$temp_dir"; then
    fail "cannot extract $archive"
fi

binary="$temp_dir/$package_name/tmup"
[ -f "$binary" ] || fail "$package_name/tmup is not a regular file"
[ ! -L "$binary" ] || fail "$package_name/tmup must not be a symbolic link"
[ -x "$binary" ] || fail "$package_name/tmup is not executable"

[ "$run_smoke_test" = true ] || exit 0

if ! actual_version=$("$binary" --version); then
    fail "$package_name/tmup --version failed"
fi
expected_version="tmup $version"
[ "$actual_version" = "$expected_version" ] ||
    fail "binary reported '$actual_version', expected '$expected_version'"
