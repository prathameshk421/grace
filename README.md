# grace

![CI](https://github.com/prathameshk421/grace/actions/workflows/ci.yml/badge.svg)

Fast travel between your project folders.

In Elden Ring, you rest at a Site of Grace and fast travel to any grace you found before. `grace` does the same for your terminal. It records every folder you `cd` into, and it tags each folder with the AI tools and editors you used there (`[claude]`, `[vscode]`, `[zed]` and more). Type `grace`, choose a folder, and your shell goes there.

```
grace ❯ knn
★ ~/Documents/Practicals/AIMLL/KMeans        [claude]          12h
  ~/Documents/Practicals/AIMLL/KNN           [antigravity]      1d
  ~/Projects/grace                           [claude]          now
↑↓ move · enter travel · esc cancel                          3/31
```

## Install

Platforms: macOS, Linux and Windows. Shells: zsh, bash, fish and PowerShell.

### Download

Get the archive for your system from the [Releases page](https://github.com/prathameshk421/grace/releases/latest).

| Your system | Archive |
|---|---|
| macOS Apple Silicon | `grace-<version>-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `grace-<version>-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `grace-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `grace-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 | `grace-<version>-x86_64-pc-windows-msvc.zip` |

macOS and Linux: extract the archive and move `grace` into a folder on your PATH, such as `~/.local/bin` or `/usr/local/bin`.

```sh
tar -xzf grace-*.tar.gz
mv grace-*/grace ~/.local/bin/
```

The macOS binary is not signed. If macOS blocks it, run this:

```sh
xattr -d com.apple.quarantine ~/.local/bin/grace
```

Windows: extract `grace.exe` into a folder on your PATH, for example `%USERPROFILE%\bin`. Add that folder to PATH if it is not there yet.

### Build from source

You need Rust.

```sh
git clone https://github.com/prathameshk421/grace
cd grace
cargo install --path .
```

### Shell setup

Add one line to your shell config.

| Shell | Line | File |
|---|---|---|
| zsh | `eval "$(grace init zsh)"` | `~/.zshrc` |
| bash | `eval "$(grace init bash)"` | `~/.bashrc` |
| fish | `grace init fish \| source` | `~/.config/fish/config.fish` |
| PowerShell | `Invoke-Expression (& grace init powershell \| Out-String)` | `$PROFILE` |

Open a new terminal. On the first run, grace imports folders from your Claude Code history (`~/.claude.json`) and from your editors.

## Use

| Command | What it does |
|---|---|
| `grace` | Open the picker. Type to filter, Enter to travel. |
| `grace knn` | Start the picker with "knn". If only one folder matches, go there at once. |
| `grace -t claude` | Show only folders where you used Claude. |
| `grace -o` | Travel, then start the AI tool you used last in that folder. |
| `grace list` | Print all folders, best first. |
| `grace pin` / `grace unpin` | Keep the current folder at the top (or remove the pin). |
| `grace forget` | Remove the current folder from grace. |
| `grace import [source]` | Import history again. Sources: `all` (default), `claude`, `vscode`, `cursor`, `windsurf`, `antigravity`, `zed`. |
| `grace tag <path> <tool> [target]` | Record a tool run. The shell hook calls this. |

Each command that takes a folder also accepts a path, for example `grace forget ~/old-project`.

## Import

`grace import` reads the recent folders of your editors and prints one line per source, for example `vscode: 12 folders`.

- `claude`: `~/.claude.json`.
- `vscode`: VS Code, VS Code Insiders and VSCodium.
- `cursor`, `windsurf`, `antigravity`: the VS Code family editors of the same name.
- `zed`: the Zed workspace database.

With `all`, grace skips sources it cannot find. With a named source, it prints "not found". grace opens editor databases read-only. It never changes your editor data. Remote folders are skipped.

## Folder argument

If you type `code ~/other` in `~/a`, grace tags `~/other`, not `~/a`. grace takes the first argument that does not start with `-`. A leading `~` and relative paths work. If the argument is a file or does not exist, grace tags the current folder.

## Add your own AI tool

Run `grace tool add` with the command name. The tag is the command in lowercase. The launch command is the command.

```sh
grace tool add kiro
grace tool add q --tag amazon-q --launch "q chat"
grace tool list
grace tool rm kiro
```

Open a new terminal after you add or remove a tool. A custom tool with the same command as a built-in tool replaces it. You cannot remove built-in tools.

Names use letters, digits, `.`, `_` and `-`, and are at most 64 characters long.

## How it works

- A zsh `chpwd` hook runs `grace add "$PWD"` in the background each time you change folder.
- A zsh `preexec` hook checks the first word of each command. For a known tool, it runs `grace tag` with the folder argument, if there is one. For other commands, it starts no process.
- Folders rank by frecency: visits and AI tool runs, weighted by how recent they are. Pinned folders come first.
- Folders that no longer exist (for example, on an unplugged SSD) are hidden, not deleted.
- Data lives in one SQLite file: `$GRACE_DATA_DIR`, else `$XDG_DATA_HOME/grace`, else `~/.local/share/grace/grace.db`.

Known AI tools: `claude`, `agy` / `antigravity`, `codex`, `gemini`, `cursor` / `cursor-agent`, `opencode`, `aider`, `copilot`, `amp`, `goose`, `crush`, `qwen`.

Known editors: `code` and `code-insiders` (tag `vscode`), `codium`, `windsurf`, `zed`, `nvim` (tag `neovim`), `vim`, `subl` (tag `sublime`), `idea`, `pycharm` and `webstorm` (tag `jetbrains`).

`grace tool list` shows the kind of each tool: ai, editor or custom. To add a tool, run `grace tool add`.

grace never tracks `/` or your home folder.

## Built with Claude

I built grace with [Claude Code](https://claude.com/claude-code). Claude Opus planned the design, wrote the shared skeleton and reviewed every part. Three Claude Sonnet agents wrote the modules in parallel: storage and ranking, the picker, and the shell integration. I gave the idea, the feature list and the final checks.

## License

MIT
