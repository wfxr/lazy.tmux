#!/bin/sh

set -eu

usage() {
    echo "usage: $0 TAG EXPECTED_COMMIT [MAIN_REF]" >&2
    exit 2
}

fail() {
    echo "invalid release tag: $*" >&2
    exit 1
}

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage

tag=$1
expected_ref=$2
main_ref=${3:-origin/main}
script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)
version=$("$script_dir/validate-version.sh" "$tag")

tag_commit=$(git rev-parse --verify "refs/tags/$tag^{commit}" 2>/dev/null) ||
    fail "tag does not resolve to a commit: $tag"
expected_commit=$(git rev-parse --verify "$expected_ref^{commit}" 2>/dev/null) ||
    fail "expected commit does not resolve: $expected_ref"

[ "$tag_commit" = "$expected_commit" ] ||
    fail "$tag points to $tag_commit instead of workflow commit $expected_commit"

main_commit=$(git rev-parse --verify "$main_ref^{commit}" 2>/dev/null) ||
    fail "main ref does not resolve: $main_ref"
git merge-base --is-ancestor "$tag_commit" "$main_commit" ||
    fail "$tag is not reachable from $main_ref"

printf '%s\n' "$version"
