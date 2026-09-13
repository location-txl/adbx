#!/bin/sh

set -eu

REPOSITORY='location-txl/adbx'
RELEASE_BASE_URL="https://github.com/${REPOSITORY}/releases"

if [ -n "${HOME:-}" ]; then
    DEFAULT_INSTALL_DIR="${HOME}/.local/bin"
else
    DEFAULT_INSTALL_DIR=''
fi

TEMP_DIR=''
STAGED_BINARY=''

die() {
    printf '✗ %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Install adbx from a GitHub Release.

Usage:
  install.sh [--version <version>] [--install-dir <directory>]

Options:
  --version <version>       Install a specific version, for example v0.1.0; default: latest stable release
  --install-dir <directory> Installation directory; default: ~/.local/bin
  -h, --help                Show this help
EOF
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"
}

normalize_tag() {
    input_tag=$1

    if [ "$input_tag" = 'latest' ]; then
        printf '%s\n' 'latest'
        return
    fi

    case "$input_tag" in
        v*) tag="$input_tag" ;;
        *) tag="v${input_tag}" ;;
    esac

    # The version is used in a URL and file name, so accept only the SemVer form used by the release workflow.
    if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
        die "Version must look like v0.1.0: $input_tag"
    fi

    printf '%s\n' "$tag"
}

resolve_latest_tag() {
    latest_url=$(curl -fsSL --retry 3 --proto '=https' --tlsv1.2 \
        -o /dev/null -w '%{url_effective}' "${RELEASE_BASE_URL}/latest") \
        || die 'Unable to fetch the latest GitHub Release'

    tag_prefix="${RELEASE_BASE_URL}/tag/"
    case "$latest_url" in
        "${tag_prefix}"*) latest_tag=${latest_url#"$tag_prefix"} ;;
        *) die 'No stable GitHub Release is available' ;;
    esac

    latest_tag=${latest_tag%%\?*}
    latest_tag=${latest_tag%%\#*}
    normalize_tag "$latest_tag"
}

detect_target() {
    os_name=$(uname -s 2>/dev/null) || die 'Unable to detect the operating system'
    machine=$(uname -m 2>/dev/null) || die 'Unable to detect the CPU architecture'

    case "${os_name}:${machine}" in
        Linux:x86_64|Linux:amd64)
            printf '%s\n' 'x86_64-unknown-linux-gnu'
            ;;
        Darwin:x86_64|Darwin:amd64)
            printf '%s\n' 'x86_64-apple-darwin'
            ;;
        Darwin:arm64|Darwin:aarch64)
            printf '%s\n' 'aarch64-apple-darwin'
            ;;
        *)
            die "Unsupported platform or architecture: ${os_name}/${machine}; use install.ps1 on Windows"
            ;;
    esac
}

cleanup() {
    if [ -n "$STAGED_BINARY" ]; then
        rm -f "$STAGED_BINARY"
    fi
    if [ -n "$TEMP_DIR" ]; then
        rm -rf "$TEMP_DIR"
    fi
}

version='latest'
install_dir="$DEFAULT_INSTALL_DIR"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || die '--version requires a value'
            version=$2
            shift 2
            ;;
        --version=*)
            version=${1#*=}
            shift
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || die '--install-dir requires a value'
            install_dir=$2
            shift 2
            ;;
        --install-dir=*)
            install_dir=${1#*=}
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "Unknown option: $1; use --help for usage"
            ;;
    esac
done

[ -n "$install_dir" ] || die 'The installation directory cannot be empty'

require_command curl
require_command grep
require_command mktemp
require_command tar
require_command uname

trap cleanup EXIT
trap 'exit 130' HUP INT TERM

target=$(detect_target)
if [ "$version" = 'latest' ]; then
    tag=$(resolve_latest_tag)
else
    tag=$(normalize_tag "$version")
fi

asset_name="adbx-${tag}-${target}.tar.gz"
download_url="${RELEASE_BASE_URL}/download/${tag}/${asset_name}"

TEMP_DIR=$(mktemp -d) || die 'Unable to create a temporary directory'
archive_path="${TEMP_DIR}/${asset_name}"
extract_dir="${TEMP_DIR}/extracted"
mkdir "$extract_dir"

printf 'Downloading %s...\n' "$asset_name"
if ! curl -fL --retry 3 --proto '=https' --tlsv1.2 \
    -o "$archive_path" "$download_url"; then
    die "Failed to download the Release asset: $download_url"
fi

# Extract and validate the binary before touching the final installation directory.
if ! tar -xzf "$archive_path" -C "$extract_dir"; then
    die 'Failed to extract the Release archive'
fi

binary_path="${extract_dir}/adbx"
[ -f "$binary_path" ] || die 'The Release archive does not contain the adbx binary'

if ! mkdir -p "$install_dir"; then
    die "Unable to create the installation directory: $install_dir"
fi

STAGED_BINARY=$(mktemp "${install_dir}/.adbx.tmp.XXXXXX") \
    || die "Unable to create a temporary file in the installation directory: $install_dir"
if ! cp "$binary_path" "$STAGED_BINARY"; then
    die 'Failed to copy the adbx binary'
fi
chmod 755 "$STAGED_BINARY"

# Replace the binary in one move after the download and extraction have succeeded.
if ! mv -f "$STAGED_BINARY" "${install_dir}/adbx"; then
    die "Failed to replace the installed binary: ${install_dir}/adbx"
fi
STAGED_BINARY=''

printf '✓ adbx installed to %s\n' "${install_dir}/adbx"
installed_version=$("${install_dir}/adbx" --version 2>/dev/null || true)
if [ -n "$installed_version" ]; then
    printf '%s\n' "$installed_version"
fi

case ":${PATH:-}:" in
    *":${install_dir}:"*) ;;
    *)
        printf 'For the current shell, run: export PATH="%s:$PATH"\n' "$install_dir"
        case "${SHELL:-}" in
            zsh) printf 'To persist it, add the command above to ~/.zshrc\n' ;;
            bash) printf 'To persist it, add the command above to ~/.bashrc\n' ;;
            *) printf 'Add the installation directory to your shell PATH to use adbx directly\n' ;;
        esac
        ;;
esac
