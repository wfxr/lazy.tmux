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

targets=$("$script_dir/release-targets.sh")

is_expected_asset() {
    candidate=$1
    for release_target in $targets; do
        if [ "$candidate" = "tmup-v${version}-${release_target}.tar.gz" ] ||
            [ "$candidate" = "tmup-v${version}-${release_target}.tar.xz" ]; then
            return 0
        fi
    done
    return 1
}

for path in "$asset_dir"/* "$asset_dir"/.[!.]* "$asset_dir"/..?*; do
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        continue
    fi
    [ -f "$path" ] && [ ! -L "$path" ] || fail "unexpected non-file asset: $(basename "$path")"
    name=$(basename "$path")
    is_expected_asset "$name" || fail "unexpected local asset: $name"
done

for target in $targets; do
    for format in gz xz; do
        name="tmup-v${version}-${target}.tar.$format"
        path="$asset_dir/$name"
        [ -f "$path" ] && [ ! -L "$path" ] || fail "missing local asset: $name"
        "$script_dir/validate-archive.sh" --structure-only "$tag" "$target" "$path"
    done
done

case "$version" in
    *-*) prerelease=true ;;
    *) prerelease=false ;;
esac

releases_endpoint='repos/{owner}/{repo}/releases?per_page=100'
release_filter='add | map(select(.tag_name == "'"$tag"'"))'

release_data() {
    filter=$1
    release_pages=$(gh api --paginate "$releases_endpoint" --slurp) || return 1
    printf '%s\n' "$release_pages" | jq --raw-output "$release_filter | $filter"
}

release_state() {
    release_data 'if length == 0 then "missing" elif length > 1 then "duplicate" elif .[0].draft then "draft" else "public" end'
}

wait_for_created_draft() {
    draft_visibility_attempts=0
    while [ "$draft_visibility_attempts" -lt 10 ]; do
        draft_visibility_state=$(release_state)
        case "$draft_visibility_state" in
            draft) return 0 ;;
            missing) ;;
            public) fail "release $tag became public before asset verification" ;;
            *) fail "created release $tag has an invalid state: $draft_visibility_state" ;;
        esac

        draft_visibility_attempts=$((draft_visibility_attempts + 1))
        [ "$draft_visibility_attempts" -lt 10 ] ||
            fail "release $tag did not become visible as a draft"
        [ "$draft_visibility_attempts" -eq 1 ] || sleep 1
    done
}

state=$(release_state)
case "$state" in
    missing)
        if [ "$prerelease" = true ]; then
            gh release create "$tag" --draft --prerelease --latest=false --generate-notes --verify-tag
        else
            gh release create "$tag" --draft --generate-notes --verify-tag
        fi
        wait_for_created_draft
        ;;
    draft) ;;
    public) fail "a public release already exists for $tag" ;;
    *) fail "expected one release for $tag, found an invalid state: $state" ;;
esac

[ "$(release_state)" = draft ] || fail "release $tag is not a draft"

current_prerelease=$(release_data '.[0].prerelease')
if [ "$current_prerelease" != "$prerelease" ]; then
    if [ "$prerelease" = true ]; then
        gh release edit "$tag" --prerelease --latest=false
    else
        gh release edit "$tag" --prerelease=false
    fi
fi

# The publish job serializes runs for this tag. Direct callers must do the same.
assert_draft() {
    [ "$(release_state)" = draft ] || fail "release $tag is no longer a draft"
}

temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/tmup-publish.XXXXXX")
trap 'rm -rf "$temporary_dir"' EXIT HUP INT TERM

release_data '.[0].assets[].name' >"$temporary_dir/remote-names"
while IFS= read -r remote_name; do
    [ -n "$remote_name" ] || continue
    if ! is_expected_asset "$remote_name"; then
        gh release delete-asset "$tag" "$remote_name" --yes
    fi
done <"$temporary_dir/remote-names"

set --
for target in $targets; do
    for format in gz xz; do
        set -- "$@" "$asset_dir/tmup-v${version}-${target}.tar.$format"
    done
done

assert_draft
gh release upload "$tag" "$@" --clobber

for path in "$@"; do
    name=$(basename "$path")
    printf '%s\tuploaded\n' "$name"
done | LC_ALL=C sort >"$temporary_dir/expected-assets"

release_data '.[0].assets[] | [.name, .state] | @tsv' |
    LC_ALL=C sort >"$temporary_dir/remote-assets"

if ! cmp -s "$temporary_dir/expected-assets" "$temporary_dir/remote-assets"; then
    diff -u "$temporary_dir/expected-assets" "$temporary_dir/remote-assets" >&2 || true
    fail "uploaded release assets do not match the expected local asset set"
fi

assert_draft
if [ "$prerelease" = true ]; then
    gh release edit "$tag" --draft=false --prerelease --latest=false
else
    gh release edit "$tag" --draft=false --prerelease=false
fi

trap - EXIT HUP INT TERM
rm -rf "$temporary_dir"
printf 'published %s\n' "$tag"
