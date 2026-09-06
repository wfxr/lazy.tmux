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
target=$("$script_dir/validate-target.sh" "$target")

if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
    echo "release binary must be an executable file: $binary" >&2
    exit 1
fi

package_name="tmup-v${version}-${target}"
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/tmup-package.XXXXXX")
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

mkdir -p "$temp_dir/$package_name" "$output_dir"
cp "$binary" "$temp_dir/$package_name/tmup"
chmod 755 "$temp_dir/$package_name/tmup"
tar -cf "$temp_dir/$package_name.tar" -C "$temp_dir" "$package_name"
gzip -n -c "$temp_dir/$package_name.tar" >"$temp_dir/$package_name.tar.gz"
xz -T1 -6 -c "$temp_dir/$package_name.tar" >"$temp_dir/$package_name.tar.xz"

# Validate both formats before publishing either package.
for format in gz xz; do
    "$script_dir/validate-archive.sh" "$tag" "$target" "$temp_dir/$package_name.tar.$format"
done
for format in gz xz; do
    archive_name="$package_name.tar.$format"
    mv -f "$temp_dir/$archive_name" "$output_dir/$archive_name"
    printf '%s\n' "$output_dir/$archive_name"
done
