#!/bin/sh

set -eu

usage() {
    echo "usage: $0 TAG TARGET BINARY [OUTPUT_DIR]" >&2
    exit 2
}

[ "$#" -ge 3 ] && [ "$#" -le 4 ] || usage

tag=$1
target=$2
binary=$3
script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)
repo_root=$(CDPATH='' cd "$script_dir/../.." && pwd)
output_dir=${4:-"$repo_root/dist"}

version=$("$script_dir/validate-version.sh" "$tag")

if ! LC_ALL=C printf '%s\n' "$target" | grep -Eq '^[A-Za-z0-9_][A-Za-z0-9_.-]*$'; then
    echo "invalid release target: $target" >&2
    exit 1
fi

if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
    echo "release binary must be an executable file: $binary" >&2
    exit 1
fi

package_name="tmup-v${version}-${target}"
archive_name="${package_name}.tar.gz"
archive_path="$output_dir/$archive_name"
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/tmup-package.XXXXXX")
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

mkdir -p "$temp_dir/$package_name" "$output_dir"
cp "$binary" "$temp_dir/$package_name/tmup"
chmod 755 "$temp_dir/$package_name/tmup"
tar -czf "$temp_dir/$archive_name" -C "$temp_dir" "$package_name"

"$script_dir/validate-archive.sh" "$tag" "$target" "$temp_dir/$archive_name"
mv -f "$temp_dir/$archive_name" "$archive_path"

printf '%s\n' "$archive_path"
