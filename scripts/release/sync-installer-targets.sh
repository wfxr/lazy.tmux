#!/bin/sh

set -eu

usage() {
    echo "usage: $0 [--check]" >&2
    exit 2
}

mode='write'
case "$#:${1:-}" in
0:) ;;
1:--check) mode='check' ;;
*) usage ;;
esac

script_dir=$(CDPATH='' cd "$(dirname "$0")" && pwd)
repo_root=$(CDPATH='' cd "$script_dir/../.." && pwd)
installer="$repo_root/install.sh"
begin_marker='# BEGIN GENERATED RELEASE TARGETS'
end_marker='# END GENERATED RELEASE TARGETS'

temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/tmup-installer-targets.XXXXXX")
trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
generated="$temp_dir/generated"
rendered="$temp_dir/install.sh"

"$script_dir/release-targets.sh" --installer >"$generated"

awk -v generated_file="$generated" -v begin_marker="$begin_marker" -v end_marker="$end_marker" '
    BEGIN {
        while ((getline line < generated_file) > 0) {
            generated = generated line "\n"
        }
        close(generated_file)
    }
    $0 == begin_marker {
        if (found_begin) {
            exit 2
        }
        found_begin = 1
        in_generated_block = 1
        print
        printf "%s", generated
        next
    }
    $0 == end_marker {
        if (!in_generated_block || found_end) {
            exit 2
        }
        found_end = 1
        in_generated_block = 0
        print
        next
    }
    !in_generated_block {
        print
    }
    END {
        if (!found_begin || !found_end || in_generated_block) {
            exit 2
        }
    }
' "$installer" >"$rendered" || {
    echo "could not locate the generated release target block in $installer" >&2
    exit 1
}

if [ "$mode" = check ]; then
    if ! cmp -s "$installer" "$rendered"; then
        echo "installer release targets are stale; run scripts/release/sync-installer-targets.sh" >&2
        exit 1
    fi
    exit 0
fi

cp "$rendered" "$installer"
chmod 755 "$installer"
printf '%s\n' "$installer"
