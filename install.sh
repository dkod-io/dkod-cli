#!/bin/sh
# install.sh — portable installer for the dkod CLI.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/dkod-io/dkod-cli/main/install.sh | sh
#
# Environment:
#   DKOD_VERSION              Specific tag to install (e.g. v1.0.0). Defaults to
#                             the newest release (falls back to prereleases when
#                             no stable release exists).
#   DKOD_PREFIX               Install directory. Defaults to $HOME/.local/bin.
#   GH_TOKEN                  GitHub token with read access. Required while the
#                             repo is private.
#
# POSIX sh — no bashisms, no pipefail. Requires: curl, tar, sed, awk.

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

# Download a release asset by its numeric ID via the GitHub API. Unlike the
# /releases/download/<tag>/<file> URL, the api.github.com asset endpoint
# preserves the Authorization header across the redirect to S3, which is the
# only reliable way to fetch assets from a private repo.
#
# $1 = asset ID
# $2 = output path
curl_asset() {
    _id="$1"
    _out="$2"
    _url="https://api.github.com/repos/$REPO/releases/assets/$_id"
    if [ -n "$AUTH_HEADER" ]; then
        curl -fsSL -H "$AUTH_HEADER" -H "Accept: application/octet-stream" -o "$_out" "$_url"
    else
        curl -fsSL -H "Accept: application/octet-stream" -o "$_out" "$_url"
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

# parse_asset_id <asset-name> — read a GitHub release JSON body on stdin and
# print the numeric ID of the asset whose "name" field matches <asset-name>.
# Within each asset object, GitHub returns "id" before "name", so we capture
# the most recent "id" we have seen and emit it the first time we see a
# matching "name".
parse_asset_id() {
    awk -v target="$1" '
        BEGIN { id = "" }
        {
            line = $0
            while (match(line, /"id"[[:space:]]*:[[:space:]]*[0-9]+/)) {
                tok = substr(line, RSTART, RLENGTH)
                gsub(/[^0-9]/, "", tok)
                id = tok
                line = substr(line, RSTART + RLENGTH)
            }
        }
        /"name"[[:space:]]*:[[:space:]]*"[^"]*"/ {
            line = $0
            while (match(line, /"name"[[:space:]]*:[[:space:]]*"[^"]*"/)) {
                tok = substr(line, RSTART, RLENGTH)
                sub(/^"name"[[:space:]]*:[[:space:]]*"/, "", tok)
                sub(/"$/, "", tok)
                if (tok == target && id != "") {
                    print id
                    exit 0
                }
                line = substr(line, RSTART + RLENGTH)
            }
        }
    '
}

# parse_first_nondraft_tag — read a JSON array body from /repos/.../releases
# on stdin and print the tag_name of the first non-draft entry. GitHub
# returns the array sorted newest-first, so the first non-draft is the
# newest non-draft release (prerelease or stable).
parse_first_nondraft_tag() {
    awk '
        BEGIN { tag = ""; draft = ""; done = 0 }
        /"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"/ {
            if (tag != "" && draft == "false") {
                print tag
                done = 1
                exit 0
            }
            line = $0
            match(line, /"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"/)
            tok = substr(line, RSTART, RLENGTH)
            sub(/^"tag_name"[[:space:]]*:[[:space:]]*"/, "", tok)
            sub(/"$/, "", tok)
            tag = tok
            draft = ""
        }
        /"draft"[[:space:]]*:[[:space:]]*(true|false)/ {
            line = $0
            match(line, /"draft"[[:space:]]*:[[:space:]]*(true|false)/)
            tok = substr(line, RSTART, RLENGTH)
            if (tok ~ /true/) draft = "true"; else draft = "false"
        }
        END {
            if (!done && tag != "" && draft == "false") print tag
        }
    '
}

# --- Resolve version ----------------------------------------------------------

if [ -n "${DKOD_VERSION:-}" ]; then
    VERSION="$DKOD_VERSION"
    log "using DKOD_VERSION=$VERSION"
else
    log "resolving latest release from GitHub..."
    latest_url="https://api.github.com/repos/$REPO/releases/latest"
    VERSION=""
    if api_body=$(curl_gh_api "$latest_url" 2>/dev/null); then
        VERSION=$(printf '%s' "$api_body" \
            | grep -m1 '"tag_name"' \
            | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
    fi

    if [ -z "$VERSION" ]; then
        # /releases/latest returns 404 when every release is a prerelease, or
        # when no releases exist. Fall back to listing all releases and
        # picking the newest non-draft entry (prereleases are eligible —
        # users running install.sh against a prerelease-only repo want the
        # prerelease as a fallback; pin DKOD_VERSION to opt out).
        log "no stable release found, falling back to newest non-draft release..."
        list_url="https://api.github.com/repos/$REPO/releases?per_page=30"
        if ! list_body=$(curl_gh_api "$list_url" 2>/dev/null); then
            if [ -z "$AUTH_HEADER" ]; then
                err "failed to query $list_url. This repo is currently private; set GH_TOKEN to a token with read access."
            else
                err "failed to query $list_url with the provided GH_TOKEN. Check token validity and scopes."
            fi
        fi
        VERSION=$(printf '%s' "$list_body" | parse_first_nondraft_tag)
        if [ -z "$VERSION" ]; then
            err "no non-draft releases found at $list_url"
        fi
        log "warning: using prerelease/non-stable release $VERSION (no stable release available)"
    else
        log "latest release: $VERSION"
    fi
fi

# --- Tempdir + cleanup --------------------------------------------------------

TMPDIR_INSTALL=$(mktemp -d 2>/dev/null || mktemp -d -t dkod-install)
cleanup() {
    rm -rf "$TMPDIR_INSTALL"
}
trap cleanup EXIT INT HUP TERM

# --- Resolve asset IDs -------------------------------------------------------

ASSET="dkod-${VERSION}-${TARGET}.tar.gz"
SUMS="${ASSET}.sha256"

log "looking up asset IDs for $VERSION..."
release_url="https://api.github.com/repos/$REPO/releases/tags/$VERSION"
if ! release_body=$(curl_gh_api "$release_url" 2>/dev/null); then
    if [ -z "$AUTH_HEADER" ]; then
        err "failed to query $release_url. This repo is currently private; set GH_TOKEN to a token with read access."
    else
        err "failed to query $release_url with the provided GH_TOKEN. Check token validity and scopes."
    fi
fi

ASSET_ID=$(printf '%s' "$release_body" | parse_asset_id "$ASSET")
if [ -z "$ASSET_ID" ]; then
    err "release $VERSION does not contain asset $ASSET"
fi

SUMS_ID=$(printf '%s' "$release_body" | parse_asset_id "$SUMS")

# --- Download ----------------------------------------------------------------

log "downloading $ASSET..."
if ! curl_asset "$ASSET_ID" "$TMPDIR_INSTALL/$ASSET"; then
    err "failed to download asset id $ASSET_ID ($ASSET)"
fi

SUMS_DOWNLOADED=0
if [ -n "$SUMS_ID" ]; then
    log "downloading $SUMS..."
    if curl_asset "$SUMS_ID" "$TMPDIR_INSTALL/$SUMS" 2>/dev/null; then
        SUMS_DOWNLOADED=1
    else
        log "warning: failed to download checksum file $SUMS; skipping verification"
    fi
else
    log "warning: checksum file $SUMS not found in release; skipping verification"
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
        expected=$(sed -E 's/^([0-9a-fA-F]+).*/\1/' < "$TMPDIR_INSTALL/$SUMS")
        actual=$(cd "$TMPDIR_INSTALL" && $SHA_TOOL "$ASSET" | sed -E 's/^([0-9a-fA-F]+).*/\1/')
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
