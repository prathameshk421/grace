#!/bin/sh
# shellcheck disable=SC2016,SC2088
# Install grace: curl -fsSL https://raw.githubusercontent.com/prathameshk421/grace/main/install.sh | sh
# Env: GRACE_VERSION (for example v1.1.1), GRACE_INSTALL_DIR (default ~/.local/bin).
set -eu

REPO="prathameshk421/grace"
RELEASES="https://github.com/$REPO/releases"

err() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || err "$1 is required but not found"
}

need curl
need tar
need uname
need mktemp

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Darwin) os_part="apple-darwin" ;;
  Linux) os_part="unknown-linux-gnu" ;;
  *) err "unsupported OS: $os. See $RELEASES for other builds." ;;
esac

case "$arch" in
  arm64 | aarch64) arch_part="aarch64" ;;
  x86_64 | amd64) arch_part="x86_64" ;;
  *) err "unsupported CPU: $arch. See $RELEASES for other builds." ;;
esac

target="$arch_part-$os_part"

version="${GRACE_VERSION:-}"
if [ -z "$version" ]; then
  # The releases/latest page redirects to .../tag/<version>. This has no API rate limit.
  latest_url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$RELEASES/latest" 2>/dev/null || true)
  case "$latest_url" in
    */tag/*) version=${latest_url##*/tag/} ;;
  esac
  if [ -z "$version" ]; then
    # Fallback: the GitHub API (rate limited for anonymous calls).
    version=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null |
      sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1 || true)
  fi
  [ -n "$version" ] || err "could not read the latest version. Set GRACE_VERSION or see $RELEASES"
fi

name="grace-$version-$target"
base="$RELEASES/download/$version"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

printf 'Downloading grace %s for %s\n' "$version" "$target"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" ||
  err "download failed: $base/$name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256" ||
  err "download failed: $base/$name.tar.gz.sha256"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null) ||
    err "checksum mismatch for $name.tar.gz"
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null) ||
    err "checksum mismatch for $name.tar.gz"
else
  err "sha256sum or shasum is required to verify the download"
fi
echo "Checksum OK"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp" || err "could not extract $name.tar.gz"
[ -f "$tmp/$name/grace" ] || err "grace binary not found in the archive"

dir="${GRACE_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$dir" || err "could not create $dir"
cp "$tmp/$name/grace" "$dir/grace" || err "could not write $dir/grace"
chmod 755 "$dir/grace"

if [ "$os" = "Darwin" ]; then
  xattr -d com.apple.quarantine "$dir/grace" >/dev/null 2>&1 || true
fi

printf 'Installed grace %s to %s/grace\n' "$version" "$dir"

case ":${PATH:-}:" in
  *":$dir:"*) ;;
  *)
    printf '\nwarning: %s is not on your PATH. Add it to your shell config:\n' "$dir"
    printf '  export PATH="%s:$PATH"\n' "$dir"
    ;;
esac

case "${SHELL:-}" in
  */zsh)
    file="~/.zshrc"
    line='eval "$(grace init zsh)"'
    ;;
  */bash)
    file="~/.bashrc"
    line='eval "$(grace init bash)"'
    ;;
  */fish)
    file="~/.config/fish/config.fish"
    line='grace init fish | source'
    ;;
  *)
    file=""
    line=""
    ;;
esac

printf '\nNext step: '
if [ -n "$file" ]; then
  printf 'add this line to %s, then open a new terminal.\n\n  %s\n' "$file" "$line"
else
  printf 'run `grace init --help` to see the line for your shell.\n'
fi
