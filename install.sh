#!/bin/sh
set -eu

REPO="lucasbp/pipetrain"
BASE="${PIPETRAIN_RELEASE_BASE:-https://github.com/$REPO/releases}"
DEST="${PIPETRAIN_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${PIPETRAIN_VERSION:-latest}"

fail() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required but not installed"
}

need uname
need tar
need mktemp

if command -v curl >/dev/null 2>&1; then
  fetch() { curl --fail --silent --show-error --location --connect-timeout 15 --max-time 120 --proto '=https,http' --output "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() {
    wget --quiet --connect-timeout=15 --timeout=120 --tries=1 --output-document="$2" "$1" &
    transfer=$!
    (
      trap 'kill "${timer_sleep:-}" 2>/dev/null || :; exit 0' TERM INT
      sleep 120 &
      timer_sleep=$!
      wait "$timer_sleep"
      kill "$transfer" 2>/dev/null || :
    ) &
    timer=$!
    status=0
    wait "$transfer" || status=$?
    kill "$timer" 2>/dev/null || :
    wait "$timer" 2>/dev/null || :
    return "$status"
  }
else
  fail "curl or wget is required"
fi

if command -v sha256sum >/dev/null 2>&1; then
  hash_of() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  hash_of() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
  fail "sha256sum or shasum is required to verify the download"
fi

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) fail "unsupported system $(uname -s); on Windows use install.ps1" ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "unsupported CPU $(uname -m)" ;;
esac

target="$arch-$os"

if [ "$VERSION" = latest ]; then
  command -v curl >/dev/null 2>&1 || fail "finding the latest release needs curl; set PIPETRAIN_VERSION to a version such as 0.2.0"
  tag=$(curl --silent --show-error --location --connect-timeout 15 --max-time 120 --proto '=https,http' --output /dev/null --write-out '%{url_effective}' "$BASE/latest" 2>/dev/null | sed 's|.*/||') || tag=""
  case "$tag" in v[0-9]*) ;; *) fail "could not find the latest release at $BASE/latest" ;; esac
else
  case "$VERSION" in v*) tag="$VERSION" ;; *) tag="v$VERSION" ;; esac
fi

asset="pipetrain-$tag-$target.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

printf 'Downloading %s\n' "$asset"
fetch "$BASE/download/$tag/$asset" "$tmp/$asset" || fail "could not download $BASE/download/$tag/$asset"
fetch "$BASE/download/$tag/SHA256SUMS" "$tmp/SHA256SUMS" || fail "could not download SHA256SUMS for $tag"

expected=$(awk -v name="$asset" '{ f = $2; sub(/^\*/, "", f); if (f == name) { print $1; exit } }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || fail "SHA256SUMS does not list $asset; nothing was installed"
actual=$(hash_of "$tmp/$asset")
[ "$expected" = "$actual" ] || fail "SHA-256 mismatch for $asset; nothing was installed"

tar -xzf "$tmp/$asset" -C "$tmp" pipetrain || fail "the archive has no pipetrain executable"
mkdir -p "$DEST"
install_path="$DEST/pipetrain"
cp "$tmp/pipetrain" "$install_path.new"
chmod 755 "$install_path.new"
mv -f "$install_path.new" "$install_path"

printf 'Installed pipetrain %s to %s\n' "$tag" "$install_path"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) printf 'Add %s to your PATH to run it: export PATH="%s:$PATH"\n' "$DEST" "$DEST" ;;
esac
