#!/bin/sh

set -eu

usage() {
    echo "usage: $0 TAG ASSET_DIR" >&2
    exit 2
}

fail() {
    echo "release publication failed: $*" >&2
    exit 1
}

[ "$#" -eq 2 ] || usage

tag=$1
asset_dir=$2
script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)
version=$("$script_dir/validate-version.sh" "$tag")

[ -d "$asset_dir" ] || fail "asset directory not found: $asset_dir"

targets='x86_64-unknown-linux-musl
aarch64-unknown-linux-musl
x86_64-apple-darwin
aarch64-apple-darwin'

is_expected_asset() {
    candidate=$1
    if [ "$candidate" = SHA256SUMS ]; then
        return 0
    fi
    for release_target in $targets; do
        if [ "$candidate" = "tmup-v${version}-${release_target}.tar.gz" ]; then
            return 0
        fi
    done
    return 1
}

asset_count=0
for path in "$asset_dir"/* "$asset_dir"/.[!.]* "$asset_dir"/..?*; do
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        continue
    fi
    [ -f "$path" ] && [ ! -L "$path" ] || fail "unexpected non-file asset: $(basename "$path")"
    name=$(basename "$path")
    is_expected_asset "$name" || fail "unexpected local asset: $name"
    asset_count=$((asset_count + 1))
done

[ "$asset_count" -eq 5 ] || fail "expected four archives and SHA256SUMS, found $asset_count assets"

expected_checksum_names=
for target in $targets; do
    name="tmup-v${version}-${target}.tar.gz"
    path="$asset_dir/$name"
    [ -f "$path" ] && [ ! -L "$path" ] || fail "missing local asset: $name"
    "$script_dir/validate-archive.sh" "$tag" "$target" "$path"
    expected_checksum_names="${expected_checksum_names}${name}
"
done

checksum_path="$asset_dir/SHA256SUMS"
[ -f "$checksum_path" ] && [ ! -L "$checksum_path" ] || fail "missing local asset: SHA256SUMS"
actual_checksum_names=$(awk 'NF { print $2 }' "$checksum_path")
[ "$actual_checksum_names" = "$(printf '%b' "$expected_checksum_names")" ] ||
    fail "SHA256SUMS must cover exactly the four release archives in target order"
(
    cd "$asset_dir"
    sha256sum --check --strict SHA256SUMS >/dev/null
) || fail "SHA256SUMS verification failed"

case "$version" in
    *-*) prerelease=true ;;
    *) prerelease=false ;;
esac

release_endpoint="repos/{owner}/{repo}/releases/tags/$tag"
if draft=$(gh api "$release_endpoint" --jq '.draft' 2>/dev/null); then
    [ "$draft" = true ] || fail "a public release already exists for $tag"
else
    if [ "$prerelease" = true ]; then
        gh release create "$tag" --draft --prerelease --latest=false --generate-notes --verify-tag
    else
        gh release create "$tag" --draft --generate-notes --verify-tag
    fi
fi

draft=$(gh api "$release_endpoint" --jq '.draft')
[ "$draft" = true ] || fail "release $tag is not a draft"

current_prerelease=$(gh api "$release_endpoint" --jq '.prerelease')
if [ "$current_prerelease" != "$prerelease" ]; then
    if [ "$prerelease" = true ]; then
        gh release edit "$tag" --prerelease --latest=false
    else
        gh release edit "$tag" --prerelease=false
    fi
fi

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/tmup-publish.XXXXXX")
trap 'rm -rf "$temporary_dir"' EXIT HUP INT TERM

gh api "$release_endpoint" --jq '.assets[].name' >"$temporary_dir/remote-names"
while IFS= read -r remote_name; do
    [ -n "$remote_name" ] || continue
    if ! is_expected_asset "$remote_name"; then
        gh release delete-asset "$tag" "$remote_name" --yes
    fi
done <"$temporary_dir/remote-names"

set --
for target in $targets; do
    set -- "$@" "$asset_dir/tmup-v${version}-${target}.tar.gz"
done
set -- "$@" "$checksum_path"

gh release upload "$tag" "$@" --clobber

for path in "$@"; do
    name=$(basename "$path")
    digest=$(sha256sum "$path" | awk '{ print $1 }')
    printf '%s\tuploaded\tsha256:%s\n' "$name" "$digest"
done | LC_ALL=C sort >"$temporary_dir/expected-assets"

gh api "$release_endpoint" --jq '.assets[] | [.name, .state, .digest] | @tsv' |
    LC_ALL=C sort >"$temporary_dir/remote-assets"

if ! cmp -s "$temporary_dir/expected-assets" "$temporary_dir/remote-assets"; then
    diff -u "$temporary_dir/expected-assets" "$temporary_dir/remote-assets" >&2 || true
    fail "uploaded release assets do not match the verified local asset set"
fi

if [ "$prerelease" = true ]; then
    gh release edit "$tag" --draft=false --prerelease --latest=false
else
    gh release edit "$tag" --draft=false --prerelease=false
fi

trap - EXIT HUP INT TERM
rm -rf "$temporary_dir"
printf 'published %s\n' "$tag"
