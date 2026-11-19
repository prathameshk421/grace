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
mac_arm=$(sum aarch64-apple-darwin)
mac_intel=$(sum x86_64-apple-darwin)
linux_arm=$(sum aarch64-unknown-linux-gnu)
linux_intel=$(sum x86_64-unknown-linux-gnu)

asset() {
  printf '      url "%s/grace-%s-%s.tar.gz"\n' "$base" "$tag" "$1"
  printf '      sha256 "%s"\n' "$2"
}

cat <<EOF
class Grace < Formula
  desc "Fast travel between project folders, with tags for the AI tools used there"
  homepage "https://github.com/prathameshk421/grace"
  license "MIT"

  on_macos do
    on_arm do
$(asset aarch64-apple-darwin "$mac_arm")
    end
    on_intel do
$(asset x86_64-apple-darwin "$mac_intel")
    end
  end

  on_linux do
    on_arm do
$(asset aarch64-unknown-linux-gnu "$linux_arm")
    end
    on_intel do
$(asset x86_64-unknown-linux-gnu "$linux_intel")
    end
  end

  def install
    bin.install "grace"
  end

  def caveats
    <<~EOS
      grace is fast travel for your terminal.
      It remembers every project folder you work in, on any disk,
      and tags each one with the AI tools and editors you used there.
      Type \`grace\` and pick a folder to jump to it.

      Set it up in 2 steps:
        1. Add one line to your shell, then open a new terminal:
             zsh:  echo 'eval "\$(grace init zsh)"' >> ~/.zshrc
             bash: echo 'eval "\$(grace init bash)"' >> ~/.bashrc
             fish: echo 'grace init fish | source' >> ~/.config/fish/config.fish
        2. Run \`grace\`. The first run imports your Claude Code and editor history.

      More to try:
        grace hub      agents, shell, live diff and file tree in one window
        grace diff     the git changes of a folder
        grace tree     the files of a folder as a tree
        grace --help   every command

      \`grace hub\` needs tmux: brew install tmux
      Docs: https://github.com/prathameshk421/grace
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/grace --version")
  end
end
EOF
