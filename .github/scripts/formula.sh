#!/usr/bin/env bash
# Print the Homebrew formula for a release.
# Usage: formula.sh <tag> <dir with the .tar.gz.sha256 files>
set -euo pipefail
tag=$1
dir=$2
base="https://github.com/prathameshk421/grace/releases/download/${tag}"

sum() {
  local f="${dir}/grace-${tag}-$1.tar.gz.sha256"
  [ -s "$f" ] || { echo "missing $f" >&2; exit 1; }
  awk '{print $1}' "$f"
}

# Read every sum first, so a missing file stops the script.
declare -A sums
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  sums[$t]=$(sum "$t")
done

asset() {
  printf '      url "%s/grace-%s-%s.tar.gz"\n' "$base" "$tag" "$1"
  printf '      sha256 "%s"\n' "${sums[$1]}"
}

cat <<EOF
class Grace < Formula
  desc "Fast travel between project folders, with tags for the AI tools used there"
  homepage "https://github.com/prathameshk421/grace"
  license "MIT"

  on_macos do
    on_arm do
$(asset aarch64-apple-darwin)
    end
    on_intel do
$(asset x86_64-apple-darwin)
    end
  end

  on_linux do
    on_arm do
$(asset aarch64-unknown-linux-gnu)
    end
    on_intel do
$(asset x86_64-unknown-linux-gnu)
    end
  end

  def install
    bin.install "grace"
  end

  def caveats
    <<~EOS
      Add grace to your shell, then open a new terminal:
        zsh:  echo 'eval "\$(grace init zsh)"' >> ~/.zshrc
        bash: echo 'eval "\$(grace init bash)"' >> ~/.bashrc
        fish: echo 'grace init fish | source' >> ~/.config/fish/config.fish

      \`grace hub\` needs tmux:
        brew install tmux
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/grace --version")
  end
end
EOF
