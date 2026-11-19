<div align="center">

<img src="assets/logo.svg" alt="grace logo" width="120" height="120">

# grace

**Fast travel for your terminal.**

[![CI](https://github.com/prathameshk421/grace/actions/workflows/ci.yml/badge.svg)](https://github.com/prathameshk421/grace/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/prathameshk421/grace)](https://github.com/prathameshk421/grace/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](#license)
![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)
[![Built with Claude Opus 5.5](https://img.shields.io/badge/Built%20with-Claude%20Opus%205.5-D97757?logo=anthropic&logoColor=white)](https://claude.com/claude-code)

[Install](#install) · [Usage](#usage) · [Tools](#tools-and-import-sources) · [How it works](#how-it-works)

<br>

<img src="assets/demo.svg" alt="The grace picker. It lists project folders with tags for the AI tools and editors used there." width="760">

</div>

## Every folder. One keystroke away.

You have projects on many disks. You forget where they are. You switch between AI tools and lose track of which one you used where. grace remembers for you.

In Elden Ring, you rest at a Site of Grace and fast travel to any grace you found. grace does the same for your terminal.

## Highlights

| | |
|---|---|
| **~13 ms** | One hook call. It runs in the background, so the prompt does not wait. |
| **13 + 11** | 13 AI tools and 11 editors built in. |
| **3 systems, 4 shells** | macOS, Linux, Windows. zsh, bash, fish, PowerShell. |
| **7 sources** | Claude Code, VS Code, Cursor, Windsurf, Antigravity, Zed and T3 Code history. |
| **0 config** | Install, add one line to your shell, and go. |
| **1 binary** | Written in Rust. Local SQLite. Nothing leaves your machine. |

## Fast travel.

Type `grace` to open the picker. Type to filter, press Enter, and your shell moves there. If your word matches only one folder, grace jumps at once.

```sh
grace          # open the picker
grace knn      # one match: go there now
```

## See which agent needs you.

Run Claude Code, Codex and other CLI agents in many projects at once. `grace sessions` lists the live ones. The agents that need you come first.

```
grace ❯
  ~/code/aiml/knn        [claude]   ● needs you     2m
  ~/code/aiml/svm        [claude]   ○ your turn     5m
  ~/work/api-server      [codex]    ◐ working       now
  ~/code/aiml/kmeans     [agy]      ✓ done          12m
```

"Needs you" means the agent is blocked on a question or a permission. "Your turn" means it finished.

```sh
grace setup claude     # Claude Code CLI reports its status to grace
grace setup codex      # Codex CLI reports when a turn ends
grace sessions         # picker with live agents only
grace sessions -l      # plain list
```

`grace setup` makes a backup of the config first. `--remove` undoes it. If Codex already has a `notify` program, `grace setup codex` keeps it and forwards each event to it. In zsh and fish, grace also sees every AI CLI start and exit, with no setup.

## Tags show where you worked.

Each folder shows the AI tools and editors you used in it. Filter by tag to see only the folders where you used one tool.

```sh
grace -t claude
```

## Pick up where you stopped.

`grace -o` travels to the folder and starts the AI tool you used there last.

```sh
grace -o
```

## History you already have.

On first run, grace imports recent folders from Claude Code, VS Code, Cursor, Windsurf, Antigravity, Zed and T3 Code. Your folder list is full on day one.

```sh
grace import           # all sources
grace import zed       # one source
```

## Your tools too.

Add any command as a tool. grace tags it and can launch it.

```sh
grace tool add kiro
grace tool add q --tag amazon-q --launch "q chat"
```

## Pin. Forget. Done.

Pin a folder to keep it at the top. Forget a folder to remove it.

```sh
grace pin
grace forget ~/old-project
```

## Private by design.

grace stores everything in a local SQLite file. It sends nothing over the network. It opens editor databases read-only and never changes your editor data. Folders that do not exist, such as an unplugged SSD, are hidden, not deleted. They return when the disk returns.

## Install

### macOS and Linux

```sh
curl -fsSL https://raw.githubusercontent.com/prathameshk421/grace/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/prathameshk421/grace/main/install.ps1 | iex
```

### Other ways

**Download.** Get the archive for your system from the [Releases page](https://github.com/prathameshk421/grace/releases/latest). Asset names have the form `grace-<version>-<target>`.

| Your system | Archive |
|---|---|
| macOS Apple Silicon | `grace-<version>-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `grace-<version>-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `grace-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `grace-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 | `grace-<version>-x86_64-pc-windows-msvc.zip` |

macOS and Linux: extract the archive and move `grace` into a folder on your PATH.

```sh
tar -xzf grace-*.tar.gz
mv grace-*/grace ~/.local/bin/
```

The macOS binary is not signed. If macOS blocks it, run this:

```sh
xattr -d com.apple.quarantine ~/.local/bin/grace
```

Windows: extract `grace.exe` into a folder on your PATH, for example `%USERPROFILE%\bin`. Add that folder to PATH if it is not there yet.

**Build from source.** You need Rust.

```sh
cargo install --git https://github.com/prathameshk421/grace
```

### Set up your shell

1. Pick your shell in the table. Add the line to the file.
2. Open a new terminal.
3. Run `grace`. On the first run, grace imports your Claude Code and editor history.

| Shell | Line | File |
|---|---|---|
| zsh | `eval "$(grace init zsh)"` | `~/.zshrc` |
| bash | `eval "$(grace init bash)"` | `~/.bashrc` |
| fish | `grace init fish \| source` | `~/.config/fish/config.fish` |
| PowerShell | `Invoke-Expression (& grace init powershell \| Out-String)` | `$PROFILE` |

## Usage

| Command | What it does |
|---|---|
| `grace` | Open the picker. Type to filter, Enter to travel. |
| `grace knn` | Start the picker with "knn". If only one folder matches, go there at once. |
| `grace -t claude` | Show only folders where you used Claude. |
| `grace -o` | Travel, then start the AI tool you used last in that folder. |
| `grace list` | Print all folders, best first. |
| `grace pin` / `grace unpin` | Keep the current folder at the top (or remove the pin). |
| `grace forget` | Remove the current folder from grace. |
| `grace import [source]` | Import history again. Sources: `all` (default), `claude`, `vscode`, `cursor`, `windsurf`, `antigravity`, `zed`, `t3code`. |
| `grace tag <path> <tool> [target]` | Record a tool run. The shell hook calls this. |
| `grace tool add` / `list` / `rm` | Add, list or remove your own tools. |
| `grace --help` | Show all commands and options. |

Each command that takes a folder also accepts a path, for example `grace forget ~/old-project`.

### Tools and import sources

**AI tools (13).** `claude`, `agy` / `antigravity`, `codex`, `gemini`, `cursor` / `cursor-agent`, `opencode`, `aider`, `copilot`, `amp`, `goose`, `crush`, `qwen`, `t3` (tag `t3code`, T3 Code).

**Editors (11).** `code` and `code-insiders` (tag `vscode`), `codium`, `windsurf`, `zed`, `nvim` (tag `neovim`), `vim`, `subl` (tag `sublime`), `idea`, `pycharm` and `webstorm` (tag `jetbrains`).

`grace tool list` shows the kind of each tool: ai, editor or custom. Open a new terminal after you add or remove a tool. A custom tool with the same command as a built-in tool replaces it. Names use letters, digits, `.`, `_` and `-`, and are at most 64 characters long.

**Import sources (7).**

- `claude`: `~/.claude.json`.
- `vscode`: VS Code, VS Code Insiders and VSCodium.
- `cursor`, `windsurf`, `antigravity`: the VS Code family editors of the same name.
- `zed`: the Zed workspace database.
- `t3code`: the T3 Code project database (`~/.t3/userdata/statev2.sqlite`). grace skips deleted projects.

With `all`, grace skips sources it cannot find. With a named source, it prints "not found". Remote folders are skipped.

## How it works

- **Hooks.** A shell hook runs `grace add "$PWD"` in the background each time you change folder. Another hook checks the first word of each command. For a known tool, it runs `grace tag`. For other commands, it starts no process.
- **Frecency.** Folders rank by visits and AI tool runs, weighted by how recent they are. Pinned folders come first.
- **Folder argument.** If you type `code ~/other` in `~/a`, grace tags `~/other`, not `~/a`. If the argument is a file or does not exist, grace tags the current folder.
- **Storage.** One SQLite file: `$GRACE_DATA_DIR`, else `$XDG_DATA_HOME/grace`, else `~/.local/share/grace/grace.db`.
- **Imports.** grace opens editor databases read-only. It never tracks `/` or your home folder.

## Built with Claude

[![Built with Claude Opus 5.5](https://img.shields.io/badge/Built%20with-Claude%20Opus%205.5-D97757?logo=anthropic&logoColor=white)](https://claude.com/claude-code)

I built grace with [Claude Code](https://claude.com/claude-code). Claude Opus 5.5 led the work: it planned the design, wrote the shared skeleton, reviewed the code and made the releases. Claude Sonnet agents wrote the modules in parallel. I gave the idea, the feature list and the product decisions.

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## License

MIT
