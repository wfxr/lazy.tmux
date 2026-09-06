#!/bin/sh

set -eu
umask 077

RELEASES_URL=https://github.com/wfxr/tmup/releases/download
LATEST_RELEASE_URL=https://api.github.com/repos/wfxr/tmup/releases/latest
LATEST_PUBLISHED_RELEASE_URL='https://api.github.com/repos/wfxr/tmup/releases?per_page=100'
# BEGIN GENERATED RELEASE TARGETS
SUPPORTED_TARGETS='x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin aarch64-apple-darwin'

release_target_for_host() {
    case "$1:$2" in
    Linux:x86_64) printf '%s\n' 'x86_64-unknown-linux-musl' ;;
    Linux:aarch64) printf '%s\n' 'aarch64-unknown-linux-musl' ;;
    Darwin:x86_64) printf '%s\n' 'x86_64-apple-darwin' ;;
    Darwin:arm64) printf '%s\n' 'aarch64-apple-darwin' ;;
    *) return 1 ;;
    esac
}
# END GENERATED RELEASE TARGETS

log() {
    log_level=$1
    log_color=$2
    shift 2
    if [ "$log_level" != erro ] && { [ "${quiet:-false}" = true ] || [ "${resolve_version:-false}" = true ]; }; then
        return 0
    fi
    if [ -t 2 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then
        printf '\033[1;%sm[%s]\033[0m %s\n' "$log_color" "$log_level" "$*" >&2
    else
        printf '[%s] %s\n' "$log_level" "$*" >&2
    fi
}

info() { log info 32 "$@"; }
warn() { log warn 33 "$@"; }
erro() { log erro 31 "$@"; }

die() {
    erro "$@"
    exit 1
}

usage() {
    cat <<'EOF'
Install the latest tmup release or an explicitly selected version.

Usage:
  install.sh [--version <VERSION>] [--include-prerelease] [--target <TARGET>] [--to <DIRECTORY>] [--force]

Options:
  --version <VERSION>   Stable or prerelease version (default: latest stable)
  --include-prerelease, --pre
                        Select the latest published release, including prereleases
  --target <TARGET>     Rust target triple (default: native host target)
  --to <DIRECTORY>      Install directory (default: ~/.local/bin)
  --resolve-version     Print the selected version without installing
  --quiet               Suppress progress, informational, and PATH messages
  --force               Replace an existing tmup binary
  --help                Show this help
EOF
}

# Each resource gets one initial attempt and at most two retries. Never let the
# downloader add its own retry loop or inherit unsafe options from user config.
download() {
    download_url=$1
    download_output=$2
    download_authorization=${3:-}
    download_progress=false
    if [ "${4:-false}" = true ] && [ "$quiet" = false ] && [ -t 2 ]; then
        download_progress=true
    fi
    download_not_found=false
    download_errors="${tmp_dir}/download-errors"
    download_attempt=1
    while :; do
        # Explicitly discard partial responses even if a downloader resumes by default.
        : >"$download_output" || return 1
        download_status=0
        download_http_status=
        case "$downloader" in
        curl)
            set -- curl --disable --fail --location --show-error \
                --proto '=https' --proto-redir '=https' --max-redirs 10 \
                --retry 0 --connect-timeout 10 --max-time 60 \
                --write-out '%{http_code}' --output "$download_output"
            if [ -n "$download_authorization" ]; then
                set -- "$@" --header "$download_authorization"
            fi
            if [ "$download_progress" = true ]; then
                set -- "$@" --progress-bar
                download_http_status=$("$@" "$download_url") || download_status=$?
            else
                set -- "$@" --silent
                download_http_status=$("$@" "$download_url" 2>"$download_errors") || download_status=$?
            fi
            ;;
        wget)
            wget_attempt || download_status=$?
            ;;
        esac
        [ "$download_status" -ne 0 ] || return 0

        download_retry=false
        case "$downloader:$download_status" in
        curl:22 | wget:8)
            case "$download_http_status" in
            404) download_not_found=true; return "$download_status" ;;
            408 | 429 | 500 | 502 | 503 | 504) download_retry=true ;;
            esac
            ;;
        curl:5 | curl:6 | curl:7 | curl:18 | curl:28 | curl:52 | curl:55 | curl:56 | curl:92 | wget:4)
            download_retry=true
            ;;
        esac
        if [ "$download_retry" = false ] || [ "$download_attempt" -ge 3 ]; then
            # Downloader diagnostics can contain signed redirect URLs or credentials.
            erro "${downloader} request failed (exit ${download_status}, HTTP ${download_http_status:-unknown})"
            return "$download_status"
        fi
        sleep "$download_attempt"
        download_attempt=$((download_attempt + 1))
    done
}

wget_attempt() {
    wget_url=$download_url
    wget_redirects=0
    wget_authorization=$download_authorization
    while :; do
        # GNU wget read-timeout is an inactivity limit, not a total transfer limit.
        # tmup upgrade also supervises the complete helper with a phase deadline.
        set -- wget --no-config --quiet --server-response --tries=1 \
            --dns-timeout=10 --connect-timeout=10 --read-timeout=60 \
            --max-redirect=0 --no-hsts --output-document "$download_output"
        if [ -n "$wget_authorization" ]; then
            set -- "$@" --header "$wget_authorization"
        fi
        : >"$download_output" || return 1
        wget_status=0
        if [ "$download_progress" = true ]; then
            # Keep response headers separate while showing native progress live.
            set -- "$@" --output-file "$download_errors" --show-progress --progress=bar:force
            LC_ALL=C "$@" "$wget_url" || wget_status=$?
        else
            LC_ALL=C "$@" "$wget_url" 2>"$download_errors" || wget_status=$?
        fi
        download_http_status=$(awk '$1 ~ /^HTTP\// { status = $2 } END { print status }' "$download_errors")
        case "$download_http_status" in
        301 | 302 | 303 | 307 | 308)
            [ "$wget_status" -eq 8 ] || return "$wget_status"
            # --https-only does not constrain nonrecursive redirects. Follow them
            # ourselves, rejecting protocol downgrades before issuing the request.
            [ "$wget_redirects" -lt 10 ] || return 8
            wget_location=$(awk 'tolower($1) == "location:" { print $2; exit }' "$download_errors")
            case "$wget_location" in
            https://*) wget_next=$wget_location ;;
            //*) wget_next="https:${wget_location}" ;;
            /*)
                wget_authority=${wget_url#https://}
                wget_authority=${wget_authority%%/*}
                wget_next="https://${wget_authority}${wget_location}"
                ;;
            *) return 8 ;;
            esac
            # Do not forward a GitHub API token to a redirect recipient.
            wget_authorization=
            wget_url=$wget_next
            wget_redirects=$((wget_redirects + 1))
            ;;
        *) return "$wget_status" ;;
        esac
    done
}

validate_version() {
    case "$version" in
    v*) version=${version#v} ;;
    esac

    case "$version" in
    *+*) die "SemVer build metadata is not supported: ${version}" ;;
    esac
    case "$version" in
    *'
'*) die "invalid tmup version: ${version}" ;;
    esac
    if ! printf '%s\n' "$version" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-((0|[1-9][0-9]*)|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(\.((0|[1-9][0-9]*)|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?$'; then
        die "invalid tmup version: ${version}"
    fi
}

validate_target() {
    case "$target" in
    *[!0-9A-Za-z_-]*) ;;
    *)
        case " ${SUPPORTED_TARGETS} " in
        *" ${target} "*) return 0 ;;
        esac
        ;;
    esac
    die "unsupported target ${target}; supported targets: ${SUPPORTED_TARGETS}"
}

# ShellCheck 0.11 can lose track of earlier function definitions in these branches.
# shellcheck disable=SC2218
detect_target() {
    if ! host_os=$(uname -s); then
        die "could not detect the host operating system; supported targets: ${SUPPORTED_TARGETS}"
    fi
    if ! host_arch=$(uname -m); then
        die "could not detect the host architecture; supported targets: ${SUPPORTED_TARGETS}"
    fi

    if [ "$host_os" = Darwin ] && [ "$host_arch" = x86_64 ]; then
        translated=
        if command -v sysctl >/dev/null 2>&1; then
            translated=$(sysctl -n sysctl.proc_translated 2>/dev/null || :)
        elif [ -x /usr/sbin/sysctl ]; then
            translated=$(/usr/sbin/sysctl -n sysctl.proc_translated 2>/dev/null || :)
        fi
        if [ "$translated" = 1 ]; then
            host_arch=arm64
        fi
    fi

    if ! target=$(release_target_for_host "$host_os" "$host_arch"); then
        die "unsupported host ${host_os}/${host_arch}; supported targets: ${SUPPORTED_TARGETS}"
    fi
}

tmp_dir=
install_tmp=
cleanup() {
    if [ -n "$install_tmp" ]; then
        rm -f "$install_tmp" || :
    fi
    if [ -n "$tmp_dir" ]; then
        rm -rf "$tmp_dir" || :
    fi
}

make_tmp_dir() {
    tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/tmup-install.XXXXXX")
}

make_install_tmp() {
    install_tmp=$(mktemp "${destination}/.tmup.XXXXXX")
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
        --version)
            [ "$#" -ge 2 ] || die "--version requires a value"
            [ -n "$2" ] || die "--version requires a non-empty value"
            version=$2
            shift 2
            ;;
        --target)
            [ "$#" -ge 2 ] || die "--target requires a value"
            [ -n "$2" ] || die "--target requires a non-empty value"
            target=$2
            shift 2
            ;;
        --include-prerelease | --pre)
            include_prerelease=true
            shift
            ;;
        --to)
            [ "$#" -ge 2 ] || die "--to requires a value"
            destination=$2
            shift 2
            ;;
        --resolve-version)
            resolve_version=true
            shift
            ;;
        --quiet)
            quiet=true
            shift
            ;;
        --force)
            force=true
            shift
            ;;
        --help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
        esac
    done
}

# ShellCheck 0.11 loses function definitions after resolve-only early returns.
# shellcheck disable=SC2218
main() {
    version=
    target=
    destination=
    force=false
    include_prerelease=false
    resolve_version=false
    quiet=false

    parse_args "$@"

    if [ "$include_prerelease" = true ] && [ -n "$version" ]; then
        die "--include-prerelease/--pre cannot be combined with --version"
    fi

    if [ -n "$version" ]; then
        validate_version
        if [ "$resolve_version" = true ]; then
            printf '%s\n' "$version"
            return
        fi
    fi

    if [ "$resolve_version" = false ]; then
        if [ -z "$destination" ]; then
            [ -n "${HOME:-}" ] || die "HOME is required when --to is omitted"
            destination="${HOME}/.local/bin"
        fi
        case "$destination" in
        -*) destination="./${destination}" ;;
        esac
        destination_binary="${destination}/tmup"

        if [ "$force" = false ] && { [ -e "$destination_binary" ] || [ -L "$destination_binary" ]; }; then
            die "${destination_binary} already exists; pass --force to replace it"
        fi
        if [ -n "$target" ]; then
            validate_target
        else
            detect_target
        fi
    fi

    if command -v curl >/dev/null 2>&1; then
        downloader=curl
    elif command -v wget >/dev/null 2>&1; then
        downloader=wget
    else
        die "curl or wget is required"
    fi

    trap cleanup 0
    trap 'exit 1' HUP INT TERM

    if ! make_tmp_dir; then
        die "could not create temporary directory"
    fi

    if [ -z "$version" ]; then
        latest_release_path="${tmp_dir}/latest-release.json"
        latest_download_status=0
        latest_authorization=
        latest_release_url=$LATEST_RELEASE_URL
        latest_release_description='latest stable release'
        if [ "$include_prerelease" = true ]; then
            latest_release_url=$LATEST_PUBLISHED_RELEASE_URL
            latest_release_description='latest published release'
        fi
        if [ -n "${GITHUB_TOKEN:-}" ]; then
            latest_authorization="Authorization: Bearer ${GITHUB_TOKEN}"
        fi
        info "Resolving the ${latest_release_description}..."
        download "$latest_release_url" "$latest_release_path" "$latest_authorization" || latest_download_status=$?
        if [ "$latest_download_status" -ne 0 ]; then
            die "failed to resolve the ${latest_release_description}"
        fi
        if [ "$include_prerelease" = true ]; then
            version=$(awk '
match($0, /"tag_name"[[:space:]]*:[[:space:]]*"/) {
    candidate = substr($0, RSTART + RLENGTH)
    sub(/".*/, "", candidate)
}
match($0, /"draft"[[:space:]]*:[[:space:]]*(true|false)/) {
    draft_field = substr($0, RSTART, RLENGTH)
    if (candidate != "" && draft_field ~ /false$/) {
        print candidate
        exit
    }
    candidate = ""
}
' "$latest_release_path")
        else
            version=$(awk '
match($0, /"tag_name"[[:space:]]*:[[:space:]]*"/) {
    value = substr($0, RSTART + RLENGTH)
    sub(/".*/, "", value)
    print value
    exit
}
' "$latest_release_path")
        fi
        [ -n "$version" ] || die "latest release response has no tag_name"
        validate_version
        if [ "$include_prerelease" = false ]; then
            case "$version" in
            *-*) die "latest release is not stable: v${version}" ;;
            esac
        fi
    fi

    if [ "$resolve_version" = true ]; then
        printf '%s\n' "$version"
        return
    fi

    archive_dir="tmup-v${version}-${target}"
    release_url="${RELEASES_URL}/v${version}"
    archive_name="${archive_dir}.tar.gz"
    if command -v xz >/dev/null 2>&1 && xz --version >/dev/null 2>&1; then
        archive_name="${archive_dir}.tar.xz"
    fi
    archive_path="${tmp_dir}/${archive_name}"
    info "Downloading ${archive_name}..."
    if download "${release_url}/${archive_name}" "$archive_path" '' true; then
        :
    else
        if [ "$download_not_found" = true ] && [ "$archive_name" = "${archive_dir}.tar.xz" ]; then
            archive_name="${archive_dir}.tar.gz"
            archive_path="${tmp_dir}/${archive_name}"
            info "xz asset not found; downloading ${archive_name}..."
            download "${release_url}/${archive_name}" "$archive_path" '' true || die "failed to download ${archive_name}"
        else
            die "failed to download ${archive_name}"
        fi
    fi

    info "Extracting ${archive_name}..."
    # Decode xz separately so tar does not need xz support of its own.
    compression=z
    case "$archive_name" in
        *.tar.xz)
            xz -dc "$archive_path" >"${tmp_dir}/archive.tar" || die "could not decompress ${archive_name}"
            archive_path="${tmp_dir}/archive.tar"
            compression=
            ;;
    esac

    members_path="${tmp_dir}/ARCHIVE_MEMBERS"
    tar -t"$compression"f "$archive_path" >"$members_path" || die "could not read ${archive_name}"
    awk -v directory="${archive_dir}/" -v binary="${archive_dir}/tmup" '
    $0 == directory {
        directories += 1
        next
    }
    $0 == binary {
        binaries += 1
        next
    }
    {
        unexpected = 1
    }
    END {
        if (unexpected || directories > 1 || binaries != 1) {
            exit 1
        }
    }
' "$members_path" || die "archive does not match the expected ${archive_dir}/tmup layout"

    # Validate entry types before extraction, including hard links and special
    # files. Member names above contain no spaces, so the final verbose field is
    # unambiguous for regular files and directories on GNU tar and BSD tar.
    types_path="${tmp_dir}/ARCHIVE_TYPES"
    LC_ALL=C tar -tv"$compression"f "$archive_path" >"$types_path" || die "could not inspect ${archive_name}"
    awk -v directory="${archive_dir}/" -v binary="${archive_dir}/tmup" '
    substr($1, 1, 1) == "d" && $NF == directory { directories++; next }
    substr($1, 1, 1) == "-" && $NF == binary { binaries++; next }
    { unexpected = 1 }
    END { if (unexpected || directories > 1 || binaries != 1) exit 1 }
' "$types_path" || die "archive must contain only a regular tmup binary and its directory"

    extract_dir="${tmp_dir}/extract"
    mkdir "$extract_dir" || die "could not prepare archive extraction"
    tar -x"$compression"f "$archive_path" -C "$extract_dir" "$archive_dir/tmup" || die "could not extract ${archive_name}"
    extracted_binary="${extract_dir}/${archive_dir}/tmup"
    [ -d "${extract_dir}/${archive_dir}" ] || die "archive does not contain the expected directory"
    [ ! -L "${extract_dir}/${archive_dir}" ] || die "archive directory must not be a symlink"
    [ -f "$extracted_binary" ] || die "archive does not contain ${archive_dir}/tmup"
    [ ! -L "$extracted_binary" ] || die "archive tmup binary must not be a symlink"
    # -x asks the kernel about mount-level execution, and rejects noexec TMPDIR.
    # Inspect executable mode bits instead; the upgrade client runs its candidate
    # only after copying it beside the destination executable.
    binary_mode=$(LC_ALL=C ls -ld "$extracted_binary") || die "could not inspect archive tmup binary"
    printf '%s\n' "$binary_mode" | awk '
    substr($1, 4, 1) ~ /[xs]/ || substr($1, 7, 1) ~ /[xs]/ || substr($1, 10, 1) ~ /[xt]/ { executable = 1 }
    END { if (!executable) exit 1 }
' || die "archive tmup binary is not executable"

    info "Installing tmup to ${destination_binary}..."
    mkdir -p "$destination" || die "could not create destination directory: ${destination}"
    if [ -d "$destination_binary" ] && [ ! -L "$destination_binary" ]; then
        die "cannot replace directory: ${destination_binary}"
    fi
    if ! make_install_tmp; then
        die "could not create installation staging file"
    fi
    cp "$extracted_binary" "$install_tmp" || die "could not stage tmup for installation"
    chmod 755 "$install_tmp" || die "could not make tmup executable"
    if [ "$force" = true ]; then
        if [ -L "$destination_binary" ]; then
            rm -f "$destination_binary" || die "could not replace symlink: ${destination_binary}"
        fi
        mv -f "$install_tmp" "$destination_binary" || die "could not install tmup to ${destination}"
    else
        ln "$install_tmp" "$destination_binary" || die "${destination_binary} already exists; pass --force to replace it"
        rm -f "$install_tmp" || die "could not remove installation staging file"
    fi
    install_tmp=

    [ "$quiet" = false ] || return 0
    info "Installed tmup v${version} to ${destination_binary}"
    case ":${PATH:-}:" in
    *":${destination}:"*) ;;
    *) warn "${destination} is not in PATH; add it to run tmup directly" ;;
    esac
}

main "$@"
