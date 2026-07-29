#!/bin/sh
# Copyright © Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT
#
# Install ROCm App on Linux (x86_64, deb or rpm hosts):
#
#   curl -fsSL https://raw.githubusercontent.com/mikeroySoft/rocm-app/main/install.sh | sh
#
# Downloads the release package for this host, verifies its published
# SHA-256, and installs it with the system package manager — which also
# installs the bundled `rocm` / `rocmd` command-line tools the app drives.
# It never installs, updates, or removes a GPU driver.
#
# Pin a version with ROCM_APP_VERSION=v0.0.1; default is the newest release.
# Windows: download the `rocm-app_<version>_x64-setup.exe` installer from
# https://github.com/mikeroySoft/rocm-app/releases instead.
set -eu

REPO="mikeroySoft/rocm-app"
TAG="${ROCM_APP_VERSION:-}"

fail() {
    echo "install.sh: $1" >&2
    exit 1
}

case "$(uname -s)" in
Linux) ;;
*) fail "this script supports Linux only; Windows users should run the -setup.exe from https://github.com/$REPO/releases" ;;
esac
case "$(uname -m)" in
x86_64 | amd64) ;;
*) fail "ROCm App supports x86_64 only (this machine reports $(uname -m))" ;;
esac
command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"

# The public releases API names the newest release; parsed with sed so the
# script needs no jq. (The HTML /releases/latest redirect lags behind the
# API after publishing — measured, it pointed at /releases while the API
# already answered — so the API is the source of truth here.)
if [ -z "$TAG" ]; then
    TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/^ *"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$TAG" ] || fail "could not resolve the newest release tag"
fi
VERSION="${TAG#v}"

# Package format follows the host's own package manager, exactly as the
# release names its artifacts.
if command -v dpkg >/dev/null 2>&1; then
    FORMAT=deb
    PKG="rocm-app_${VERSION}_amd64.deb"
elif command -v rpm >/dev/null 2>&1; then
    FORMAT=rpm
    PKG="rocm-app-${VERSION}-1.x86_64.rpm"
else
    fail "neither dpkg nor rpm is available; download a package manually from https://github.com/$REPO/releases"
fi

URL="https://github.com/$REPO/releases/download/$TAG/$PKG"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

echo "downloading $PKG ($TAG)"
curl -fSL --proto '=https' "$URL" -o "$TMP/$PKG"
curl -fsSL --proto '=https' "$URL.sha256" -o "$TMP/$PKG.sha256"
(cd "$TMP" && sha256sum -c "$PKG.sha256" >/dev/null) || fail "checksum mismatch for $PKG; refusing to install"
echo "checksum verified"

as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        fail "root is required to install packages and no sudo is available"
    fi
}

# The package manager resolves dependencies and records ownership of
# /usr/bin/rocm{,d,-app}; the maintainer scripts refuse to overwrite an
# unowned binary already sitting there and name the fix if they do.
case "$FORMAT" in
deb)
    if command -v apt-get >/dev/null 2>&1; then
        as_root apt-get install -y "$TMP/$PKG"
    else
        as_root dpkg -i "$TMP/$PKG"
    fi
    ;;
rpm)
    if command -v dnf >/dev/null 2>&1; then
        as_root dnf install -y "$TMP/$PKG"
    elif command -v zypper >/dev/null 2>&1; then
        as_root zypper --non-interactive install --allow-unsigned-rpm "$TMP/$PKG"
    else
        as_root rpm -U "$TMP/$PKG"
    fi
    ;;
esac

echo "installed: launch \"ROCm\" from your applications menu, or run rocm-app"
