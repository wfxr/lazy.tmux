#!/bin/sh

set -eu

usage() {
    echo "usage: $0 [--github-matrix|--installer]" >&2
    exit 2
}

release_target_records() {
    cat <<'EOF'
x86_64-unknown-linux-musl|ubuntu-24.04||Linux|x86_64
aarch64-unknown-linux-musl|ubuntu-24.04-arm||Linux|aarch64
x86_64-apple-darwin|macos-15-intel|10.12|Darwin|x86_64
aarch64-apple-darwin|macos-15|11.0|Darwin|arm64
EOF
}

[ "$#" -le 1 ] || usage
records=$(release_target_records)

case "${1:-}" in
"")
    printf '%s\n' "$records" | awk -F '|' '{ print $1 }'
    ;;
--github-matrix)
    printf '%s\n' "$records" | awk -F '|' '
        BEGIN {
            printf "{\"include\":["
        }
        {
            if (NR > 1) {
                printf ","
            }
            printf "{\"target\":\"%s\",\"runner\":\"%s\",\"macos_deployment_target\":\"%s\"}", $1, $2, $3
        }
        END {
            print "]}"
        }
    '
    ;;
--installer)
    printf '%s\n' "$records" | awk -F '|' '
        {
            targets = targets (NR > 1 ? " " : "") $1
            target[NR] = $1
            host_os[NR] = $4
            host_arch[NR] = $5
        }
        END {
            single_quote = sprintf("%c", 39)
            print "SUPPORTED_TARGETS=" single_quote targets single_quote
            print ""
            print "release_target_for_host() {"
            print "    case \"$1:$2\" in"
            for (record = 1; record <= NR; record += 1) {
                printf "    %s:%s) printf %s%%s\\n%s %s%s%s ;;\n", host_os[record], host_arch[record], single_quote, single_quote, single_quote, target[record], single_quote
            }
            print "    *) return 1 ;;"
            print "    esac"
            print "}"
        }
    '
    ;;
*) usage ;;
esac
