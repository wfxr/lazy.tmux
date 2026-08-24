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

for path in "$asset_dir"/* "$asset_dir"/.[!.]* "$asset_dir"/..?*; do
    if [ ! -e "$path" ] && [ ! -L "$path" ]; then
        continue
    fi
    [ -f "$path" ] && [ ! -L "$path" ] || fail "unexpected non-file asset: $(basename "$path")"
    name=$(basename "$path")
    is_expected_asset "$name" || fail "unexpected local asset: $name"
done

expected_checksum_names=
for target in $targets; do
    name="tmup-v${version}-${target}.tar.gz"
    path="$asset_dir/$name"
    [ -f "$path" ] && [ ! -L "$path" ] || fail "missing local asset: $name"
    "$script_dir/validate-archive.sh" --structure-only "$tag" "$target" "$path"
    expected_checksum_names="${expected_checksum_names}${name}
"
done

checksum_path="$asset_dir/SHA256SUMS"
[ -f "$checksum_path" ] && [ ! -L "$checksum_path" ] || fail "missing local asset: SHA256SUMS"
actual_checksum_names=$(awk 'NF { print $2 }' "$checksum_path")
[ "$actual_checksum_names" = "$(printf '%b' "$expected_checksum_names")" ] ||
    fail "SHA256SUMS must cover every release archive in target order"
(
    cd "$asset_dir"
    sha256sum --check --strict SHA256SUMS >/dev/null
) || fail "SHA256SUMS verification failed"

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

state=$(release_state)
case "$state" in
    missing)
        if [ "$prerelease" = true ]; then
            gh release create "$tag" --draft --prerelease --latest=false --generate-notes --verify-tag
        else
            gh release create "$tag" --draft --generate-notes --verify-tag
        fi
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

run_id=${GITHUB_RUN_ID:?GITHUB_RUN_ID is required}
run_attempt=${GITHUB_RUN_ATTEMPT:?GITHUB_RUN_ATTEMPT is required}
lock_label="tmup-publication-run-${run_id}-attempt-${run_attempt}"
checksum_digest=$(sha256sum "$checksum_path" | awk '{ print $1 }')

release_lock() {
    release_data '.[0].assets[] | select(.name == "SHA256SUMS") | [(.label // ""), .digest] | @tsv'
}

assert_draft() {
    [ "$(release_state)" = draft ] || fail "release $tag is no longer a draft"
}

lock_record=$(release_lock)
lock_owned=false
if [ -n "$lock_record" ]; then
    lock_owner=$(printf '%s\n' "$lock_record" | cut -f 1)
    lock_digest=$(printf '%s\n' "$lock_record" | cut -f 2)
    case "$lock_owner" in
        tmup-publication-run-*-attempt-*) ;;
        *) fail "draft SHA256SUMS has no valid publication owner" ;;
    esac

    owner=${lock_owner#tmup-publication-run-}
    owner_run=${owner%%-attempt-*}
    owner_attempt=${owner##*-attempt-}
    case "$owner_run:$owner_attempt" in
        *[!0-9:]* | :* | *:) fail "draft SHA256SUMS has an invalid publication owner" ;;
    esac

    if [ "$owner_run" = "$run_id" ]; then
        if [ "$owner_attempt" = "$run_attempt" ] &&
            [ "$lock_digest" = "sha256:$checksum_digest" ]; then
            lock_owned=true
        else
            assert_draft
            gh release delete-asset "$tag" SHA256SUMS --yes
        fi
    else
        owner_status=$(gh api "repos/{owner}/{repo}/actions/runs/$owner_run" --jq '.status')
        if [ "$owner_status" != completed ]; then
            fail "publication run $owner_run is still active"
        fi
        assert_draft
        gh release delete-asset "$tag" SHA256SUMS --yes
    fi
fi

if [ "$lock_owned" = false ]; then
    assert_draft
    gh release upload "$tag" "$checksum_path#$lock_label"
    expected_lock="${lock_label}\tsha256:${checksum_digest}"
    [ "$(release_lock)" = "$(printf '%b' "$expected_lock")" ] ||
        fail "failed to acquire draft publication ownership"
fi

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
    set -- "$@" "$asset_dir/tmup-v${version}-${target}.tar.gz"
done

assert_draft
gh release upload "$tag" "$@" --clobber
set -- "$@" "$checksum_path"

for path in "$@"; do
    name=$(basename "$path")
    digest=$(sha256sum "$path" | awk '{ print $1 }')
    printf '%s\tuploaded\tsha256:%s\n' "$name" "$digest"
done | LC_ALL=C sort >"$temporary_dir/expected-assets"

release_data '.[0].assets[] | [.name, .state, .digest] | @tsv' |
    LC_ALL=C sort >"$temporary_dir/remote-assets"

if ! cmp -s "$temporary_dir/expected-assets" "$temporary_dir/remote-assets"; then
    diff -u "$temporary_dir/expected-assets" "$temporary_dir/remote-assets" >&2 || true
    fail "uploaded release assets do not match the verified local asset set"
fi

assert_draft
expected_lock="${lock_label}\tsha256:${checksum_digest}"
[ "$(release_lock)" = "$(printf '%b' "$expected_lock")" ] ||
    fail "draft publication ownership changed before publish"

if [ "$prerelease" = true ]; then
    gh release edit "$tag" --draft=false --prerelease --latest=false
else
    gh release edit "$tag" --draft=false --prerelease=false
fi

trap - EXIT HUP INT TERM
rm -rf "$temporary_dir"
printf 'published %s\n' "$tag"
