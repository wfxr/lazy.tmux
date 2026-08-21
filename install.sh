#!/bin/sh

set -eu
umask 077

RELEASES_URL=https://github.com/wfxr/tmup/releases/download

die() {
    printf 'tmup installer: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Install an explicit tmup release.

Usage:
  install.sh --version <VERSION> --target <TARGET> --to <DIRECTORY> [--force]

Options:
  --version <VERSION>  Stable or prerelease version to install
  --target <TARGET>    Supported Rust target triple to install
  --to <DIRECTORY>     Directory in which to install tmup
  --force              Replace an existing tmup binary
  --help               Show this help
EOF
}

download() {
    case "$downloader" in
    curl) curl --fail --location --silent --show-error --output "$2" "$1" ;;
    wget) wget --quiet --output-document "$2" "$1" ;;
    esac
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
            version=$2
            shift 2
            ;;
        --target)
            [ "$#" -ge 2 ] || die "--target requires a value"
            target=$2
            shift 2
            ;;
        --to)
            [ "$#" -ge 2 ] || die "--to requires a value"
            destination=$2
            shift 2
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

main() {
    version=
    target=
    destination=
    force=false

    parse_args "$@"

    [ -n "$version" ] || die "--version is required"
    [ -n "$target" ] || die "--target is required"
    [ -n "$destination" ] || die "--to is required"
    case "$destination" in
    -*) destination="./${destination}" ;;
    esac
    destination_binary="${destination}/tmup"

    if [ "$force" = false ] && { [ -e "$destination_binary" ] || [ -L "$destination_binary" ]; }; then
        die "${destination_binary} already exists; pass --force to replace it"
    fi

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

    case "$target" in
    x86_64-unknown-linux-musl | aarch64-unknown-linux-musl | x86_64-apple-darwin | aarch64-apple-darwin) ;;
    *)
        die "unsupported target ${target}; supported targets: x86_64-unknown-linux-musl, aarch64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin"
        ;;
    esac

    archive_dir="tmup-v${version}-${target}"
    archive_name="${archive_dir}.tar.gz"
    release_url="${RELEASES_URL}/v${version}"

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
    archive_path="${tmp_dir}/${archive_name}"
    checksums_path="${tmp_dir}/SHA256SUMS"
    expected_checksum_path="${tmp_dir}/EXPECTED_SHA256SUM"

    download "${release_url}/${archive_name}" "$archive_path" || die "failed to download ${archive_name}"
    download "${release_url}/SHA256SUMS" "$checksums_path" || die "failed to download SHA256SUMS"

    awk -v archive="$archive_name" '
    $2 == archive || $2 == "*" archive {
        print $1 "  " archive
        found = 1
        exit
    }
    END {
        if (!found) {
            exit 1
        }
    }
' "$checksums_path" >"$expected_checksum_path" || die "SHA256SUMS has no entry for ${archive_name}"

    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$tmp_dir" && sha256sum -c "${expected_checksum_path##*/}") || die "checksum verification failed for ${archive_name}"
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$tmp_dir" && shasum -a 256 -c "${expected_checksum_path##*/}") || die "checksum verification failed for ${archive_name}"
    else
        die "sha256sum or shasum is required"
    fi

    members_path="${tmp_dir}/ARCHIVE_MEMBERS"
    tar -tzf "$archive_path" >"$members_path" || die "could not read ${archive_name}"
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

    extract_dir="${tmp_dir}/extract"
    mkdir "$extract_dir" || die "could not prepare archive extraction"
    tar -xzf "$archive_path" -C "$extract_dir" "$archive_dir/tmup" || die "could not extract ${archive_name}"
    extracted_binary="${extract_dir}/${archive_dir}/tmup"
    [ -d "${extract_dir}/${archive_dir}" ] || die "archive does not contain the expected directory"
    [ ! -L "${extract_dir}/${archive_dir}" ] || die "archive directory must not be a symlink"
    [ -f "$extracted_binary" ] || die "archive does not contain ${archive_dir}/tmup"
    [ ! -L "$extracted_binary" ] || die "archive tmup binary must not be a symlink"
    [ -x "$extracted_binary" ] || die "archive tmup binary is not executable"

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

    printf 'Installed tmup v%s to %s/tmup\n' "$version" "$destination"
}

main "$@"
