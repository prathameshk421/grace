#!/usr/bin/env bash
# Print the Homebrew formula for grace.
# Usage: formula.sh <tag> <dir with the .tar.gz.sha256 files>
set -euo pipefail

tag=${1:?usage: formula.sh <tag> <sha-dir>}
dir=${2:?usage: formula.sh <tag> <sha-dir>}

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.+-]+)?$ ]]; then
  echo "error: invalid tag format: $tag" >&2
  exit 1
fi

ver=${tag#v}
base="https://github.com/prathameshk421/grace/releases/download/${tag}"

sha() {
  local f="${dir}/grace-${tag}-$1.tar.gz.sha256"
  if [[ ! -f "$f" ]]; then
    echo "error: missing checksum file $f" >&2
    exit 1
  fi
  local h
  h=$(awk '{print $1}' "$f" | tr '[:upper:]' '[:lower:]')
  if [[ ! "$h" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: invalid sha256 in $f: $h" >&2
    exit 1
  fi
  printf '%s' "$h"
}

cat <<EOF
class Grace < Formula
  desc "Site of Grace for project navigation, AI coding tools, and tmux workspaces"
  homepage "https://github.com/prathameshk421/grace"
  version "${ver}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "${base}/grace-${tag}-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    else
      url "${base}/grace-${tag}-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "${base}/grace-${tag}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-gnu)"
    else
      url "${base}/grace-${tag}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-gnu)"
    end
  end

  def install
    bin.install "grace"
  end

  def caveats
    <<~EOS
      Add one line to your shell config, then open a new terminal:

        zsh:   eval "\$(grace init zsh)"
        bash:  eval "\$(grace init bash)"
        fish:  grace init fish | source

      Optional completions and health diagnostics:
        grace completions zsh
        grace setup --all
        grace doctor
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/grace --version")
  end
end
EOF
