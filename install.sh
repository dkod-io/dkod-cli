#!/bin/sh
# install.sh — portable installer for the dkod CLI.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh
#
# Environment:
#   DKOD_VERSION   Specific tag to install (e.g. v1.0.0). Defaults to latest release.
#   DKOD_PREFIX    Install directory. Defaults to $HOME/.local/bin.
#   GH_TOKEN       GitHub token with read access. Required while the repo is private.
#
# POSIX sh — no bashisms, no pipefail.

set -eu

REPO="dkod-io/dkod-cli"
PREFIX="${DKOD_PREFIX:-$HOME/.local/bin}"

log() {
    printf '%s\n' "$*" >&2
}

err() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# --- Detect OS / arch ---------------------------------------------------------

uname_s=$(uname -s)
uname_m=$(uname -m)

case "$uname_s" in
    Darwin)
        case "$uname_m" in
            arm64)  TARGET="aarch64-apple-darwin" ;;
            x86_64) TARGET="x86_64-apple-darwin" ;;
            *) err "unsupported macOS arch: $uname_m" ;;
        esac
        ;;
    Linux)
        case "$uname_m" in
            aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
            x86_64)        TARGET="x86_64-unknown-linux-gnu" ;;
            *) err "unsupported Linux arch: $uname_m" ;;
        esac
        ;;
    *)
        err "unsupported OS: $uname_s (only Darwin and Linux are supported)"
        ;;
esac

log "detected target: $TARGET"

# --- Auth header --------------------------------------------------------------

AUTH_HEADER=""
if [ -n "${GH_TOKEN:-}" ]; then
    AUTH_HEADER="Authorization: Bearer $GH_TOKEN"
fi

curl_gh() {
    # $1 = URL, remaining args appended to curl
    _url="$1"
    shift
    if [ -n "$AUTH_HEADER" ]; then
        curl -fsSL -H "$AUTH_HEADER" -H "Accept: application/octet-stream" "$@" "$_url"
    else
        curl -fsSL -H "Accept: application/octet-stream" "$@" "$_url"
    fi
}

curl_gh_api() {
    _url="$1"
    if [ -n "$AUTH_HEADER" ]; then
        curl -fsSL -H "$AUTH_HEADER" -H "Accept: application/vnd.github+json" "$_url"
    else
        curl -fsSL -H "Accept: application/vnd.github+json" "$_url"
    fi
}

# --- Resolve version ----------------------------------------------------------

if [ -n "${DKOD_VERSION:-}" ]; then
    VERSION="$DKOD_VERSION"
    log "using DKOD_VERSION=$VERSION"
else
    log "resolving latest release from GitHub..."
    api_url="https://api.github.com/repos/$REPO/releases/latest"
    if ! api_body=$(curl_gh_api "$api_url" 2>/dev/null); then
        if [ -z "$AUTH_HEADER" ]; then
            err "failed to query $api_url. This repo is currently private; set GH_TOKEN to a token with read access."
        else
            err "failed to query $api_url with the provided GH_TOKEN. Check token validity and scopes."
        fi
    fi
    VERSION=$(printf '%s' "$api_body" \
        | grep -m1 '"tag_name"' \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
    if [ -z "$VERSION" ]; then
        err "could not parse tag_name from GitHub API response"
    fi
    log "latest release: $VERSION"
fi

# --- Tempdir + cleanup --------------------------------------------------------

TMPDIR_INSTALL=$(mktemp -d 2>/dev/null || mktemp -d -t dkod-install)
cleanup() {
    rm -rf "$TMPDIR_INSTALL"
}
trap cleanup EXIT INT HUP TERM

# --- Download ----------------------------------------------------------------

ASSET="dkod-${VERSION}-${TARGET}.tar.gz"
SUMS="${ASSET}.sha256"
BASE_URL="https://github.com/$REPO/releases/download/$VERSION"

log "downloading $ASSET..."
if ! curl_gh "$BASE_URL/$ASSET" -o "$TMPDIR_INSTALL/$ASSET"; then
    err "failed to download $BASE_URL/$ASSET"
fi

log "downloading $SUMS..."
SUMS_DOWNLOADED=1
if ! curl_gh "$BASE_URL/$SUMS" -o "$TMPDIR_INSTALL/$SUMS" 2>/dev/null; then
    log "warning: checksum file $SUMS not found; skipping verification"
    SUMS_DOWNLOADED=0
fi

# --- Verify checksum ---------------------------------------------------------

if [ "$SUMS_DOWNLOADED" = "1" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
        SHA_TOOL="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        SHA_TOOL="shasum -a 256"
    else
        SHA_TOOL=""
    fi

    if [ -z "$SHA_TOOL" ]; then
        log "warning: neither sha256sum nor shasum found; skipping checksum verification"
    else
        expected=$(awk '{print $1}' < "$TMPDIR_INSTALL/$SUMS")
        actual=$(cd "$TMPDIR_INSTALL" && $SHA_TOOL "$ASSET" | awk '{print $1}')
        if [ "$expected" != "$actual" ]; then
            err "checksum mismatch for $ASSET (expected $expected, got $actual)"
        fi
        log "checksum ok"
    fi
fi

# --- Extract -----------------------------------------------------------------

log "extracting..."
tar -xzf "$TMPDIR_INSTALL/$ASSET" -C "$TMPDIR_INSTALL"

if [ ! -f "$TMPDIR_INSTALL/dkod" ]; then
    err "tarball did not contain expected 'dkod' binary"
fi

# --- Install -----------------------------------------------------------------

mkdir -p "$PREFIX"
INSTALL_PATH="$PREFIX/dkod"
mv "$TMPDIR_INSTALL/dkod" "$INSTALL_PATH"
chmod +x "$INSTALL_PATH"

# --- Done --------------------------------------------------------------------

cat <<EOF
dkod $VERSION installed to $INSTALL_PATH

Make sure $PREFIX is on your PATH:
  export PATH="$PREFIX:\$PATH"

Try:  dkod --help
EOF
