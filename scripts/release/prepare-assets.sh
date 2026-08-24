#!/bin/sh

set -eu

usage() {
    echo "usage: $0 TAG ARTIFACT_DIR OUTPUT_DIR" >&2
    exit 2
}

fail() {
    echo "invalid release asset set: $*" >&2
    exit 1
}

[ "$#" -eq 3 ] || usage

tag=$1
artifact_dir=$2
output_dir=$3
script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)
version=$("$script_dir/validate-version.sh" "$tag")

[ -d "$artifact_dir" ] || fail "artifact directory not found: $artifact_dir"

targets=$("$script_dir/release-targets.sh")

asset_count=0
for path in "$artifact_dir"/* "$artifact_dir"/.[!.]* "$artifact_dir"/..?*; do
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        continue
    fi

    [ -f "$path" ] && [ ! -L "$path" ] || fail "unexpected non-file asset: $(basename "$path")"
    name=$(basename "$path")
    expected=false
    for target in $targets; do
        if [ "$name" = "tmup-v${version}-${target}.tar.gz" ]; then
            expected=true
            break
        fi
    done
    [ "$expected" = true ] || fail "unexpected asset: $name"
    asset_count=$((asset_count + 1))
done

for target in $targets; do
    name="tmup-v${version}-${target}.tar.gz"
    source_path="$artifact_dir/$name"
    [ -f "$source_path" ] && [ ! -L "$source_path" ] || fail "missing asset: $name"
done

[ "$asset_count" -eq 4 ] || fail "expected four archives, found $asset_count"

if [ -e "$output_dir" ]; then
    [ -d "$output_dir" ] || fail "output path is not a directory: $output_dir"
    rmdir "$output_dir" 2>/dev/null || fail "output directory must be empty: $output_dir"
fi

output_parent=$(dirname "$output_dir")
mkdir -p "$output_parent"
staging_dir=$(mktemp -d "$output_parent/.tmup-release.XXXXXX")
trap 'rm -rf "$staging_dir"' EXIT HUP INT TERM

for target in $targets; do
    name="tmup-v${version}-${target}.tar.gz"
    source_path="$artifact_dir/$name"
    "$script_dir/validate-archive.sh" "$tag" "$target" "$source_path"
    cp "$source_path" "$staging_dir/$name"
done

(
    cd "$staging_dir"
    for target in $targets; do
        sha256sum "tmup-v${version}-${target}.tar.gz"
    done
) >"$staging_dir/SHA256SUMS"

(
    cd "$staging_dir"
    sha256sum --check SHA256SUMS >/dev/null
)

mv "$staging_dir" "$output_dir"
trap - EXIT HUP INT TERM

printf '%s\n' "$output_dir"
