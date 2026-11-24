#!/bin/sh
# Install grace for Linux or macOS from GitHub Releases.
# Env: GRACE_VERSION (for example v2.1.0), GRACE_INSTALL_DIR (default ~/.local/bin).
set -eu

REPO="prathameshk421/grace"
RELEASES="https://github.com/${REPO}/releases"

tmp=""
cleanup() {
  if [ -n "$tmp" ] && [ -d "$tmp" ]; then
    rm -rf "$tmp"
  fi
}
trap cleanup EXIT INT TERM

use_color() {
  [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${CLICOLOR:-}" != "0" ] && [ "${TERM:-}" != "dumb" ]
}

say() { printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
  command -v "$1" >/dev/null 2>&1 || err "need \`$1\` installed"
}

fetch() {
  curl -fsSL --proto '=https' --tlsv1.2 "$@"
}

valid_version() {
  printf '%s' "$1" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.+-]+)?$'
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    err "need sha256sum, shasum, or openssl to verify download integrity"
  fi
}

need curl
need tar
need uname
need mktemp

os=$(uname -s)
arch=$(uname -m)

primary=""
fallback=""
case "$os:$arch" in
  Darwin:arm64 | Darwin:aarch64)
    primary="aarch64-apple-darwin"
    ;;
  Darwin:x86_64)
    primary="x86_64-apple-darwin"
    ;;
  Linux:x86_64 | Linux:amd64)
    primary="x86_64-unknown-linux-musl"
    fallback="x86_64-unknown-linux-gnu"
    ;;
  Linux:aarch64 | Linux:arm64)
    primary="aarch64-unknown-linux-musl"
    fallback="aarch64-unknown-linux-gnu"
    ;;
  *)
    err "unsupported system $os/$arch. Build from source with: cargo install --git https://github.com/${REPO}"
    ;;
esac

version="${GRACE_VERSION:-}"
if [ -z "$version" ]; then
  # Resolve /releases/latest redirect first to avoid GitHub API IP rate limits.
  latest_url=$(curl -fsSL --proto '=https' --tlsv1.2 -o /dev/null -w '%{url_effective}' "${RELEASES}/latest" 2>/dev/null || true)
  case "$latest_url" in
    *"/releases/tag/"*)
      version="${latest_url##*/releases/tag/}"
      ;;
  esac
  if [ -z "$version" ]; then
    version=$(
      fetch "https://api.github.com/repos/${REPO}/releases/latest" \
        | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | sed -n '1p'
    )
  fi
  [ -n "$version" ] || err "could not read the latest version. Set GRACE_VERSION or see ${RELEASES}"
fi

valid_version "$version" || err "invalid version format: ${version}"

tmp=$(mktemp -d)
target="$primary"
name="grace-${version}-${target}"
archive="${name}.tar.gz"
base="${RELEASES}/download/${version}"

if ! fetch -o "${tmp}/${archive}" "${base}/${archive}"; then
  if [ -n "$fallback" ]; then
    target="$fallback"
    name="grace-${version}-${target}"
    archive="${name}.tar.gz"
    fetch -o "${tmp}/${archive}" "${base}/${archive}" \
      || err "could not download ${archive} from ${base}"
  else
    err "could not download ${archive} from ${base}"
  fi
fi

fetch -o "${tmp}/${archive}.sha256" "${base}/${archive}.sha256" \
  || err "could not download checksum ${archive}.sha256"

expected=$(tr -d '\r\n' < "${tmp}/${archive}.sha256" | awk '{print $1}' | tr '[:upper:]' '[:lower:]')
printf '%s' "$expected" | grep -Eq '^[0-9a-f]{64}$' || err "malformed SHA-256 checksum file"

actual=$(sha256_file "${tmp}/${archive}" | tr '[:upper:]' '[:lower:]')
[ "$actual" = "$expected" ] || err "SHA-256 verification failed for ${archive} (expected ${expected}, got ${actual})"

tar -xzf "${tmp}/${archive}" -C "$tmp" "${name}/grace" \
  || err "archive ${archive} did not contain expected binary ${name}/grace"

extracted="${tmp}/${name}/grace"
[ -f "$extracted" ] && [ ! -L "$extracted" ] || err "extracted binary is missing or not a regular file"

dir="${GRACE_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$dir"
stage=$(mktemp "${dir}/.grace.tmp.XXXXXX")
cp "$extracted" "$stage"
chmod 755 "$stage"
mv -f "$stage" "${dir}/grace"

if use_color; then
  c_gold='\033[1;33m'
  c_green='\033[32m'
  c_cyan='\033[36m'
  c_dim='\033[2m'
  c_reset='\033[0m'
else
  c_gold=''
  c_green=''
  c_cyan=''
  c_dim=''
  c_reset=''
fi

say ""
printf "  ${c_green}✓${c_reset} ${c_gold}✦ grace ${version}${c_reset} installed to ${c_cyan}%s/grace${c_reset} ${c_dim}(%s)${c_reset}\n" "$dir" "$target"
say ""

case ":${PATH}:" in
  *":${dir}:"*) ;;
  *)
    say "  1. Add ${dir} to your PATH:"
    say "       export PATH=\"${dir}:\$PATH\""
    say ""
    ;;
esac

if [ "$os" = "Darwin" ]; then
  bash_rc=".bash_profile"
else
  bash_rc=".bashrc"
fi

say "  2. Enable shell tracking in your shell config:"
say "       zsh   (~/.zshrc):                    eval \"\$(grace init zsh)\""
say "       bash  (~/${bash_rc}):       eval \"\$(grace init bash)\""
say "       fish  (~/.config/fish/config.fish):  grace init fish | source"
say ""
say "  3. Connect your CLI agents and verify your setup:"
say "       grace setup --all"
say "       grace doctor"
say ""
