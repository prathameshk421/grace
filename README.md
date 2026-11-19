# grace

Fast travel between your project folders.

In Elden Ring, you rest at a Site of Grace and fast travel to any grace you found before. `grace` does the same for your terminal. It records every folder you `cd` into, and it tags each folder with the AI tools you ran there (`[claude]`, `[antigravity]`, `[codex]` and more). Type `grace`, choose a folder, and your shell goes there.

```
grace ❯ knn
★ ~/Documents/Practicals/AIMLL/KMeans        [claude]          12h
  ~/Documents/Practicals/AIMLL/KNN           [antigravity]      1d
  ~/Projects/grace                           [claude]          now
↑↓ move · enter travel · esc cancel                          3/31
```

## Install

You need Rust and zsh.

```sh
git clone https://github.com/prathameshk421/grace
cd grace
cargo install --path .
echo 'eval "$(grace init zsh)"' >> ~/.zshrc
```

Open a new terminal. On the first run, grace imports the folders from your Claude Code history (`~/.claude.json`).

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
| `grace import` | Import Claude Code history again. |

Each command that takes a folder also accepts a path, for example `grace forget ~/old-project`.

## How it works

- A zsh `chpwd` hook runs `grace add "$PWD"` in the background each time you change folder.
- A zsh `preexec` hook checks the first word of each command. For a known AI tool, it runs `grace tag`. For other commands, it starts no process.
- Folders rank by frecency: visits and AI tool runs, weighted by how recent they are. Pinned folders come first.
- Folders that no longer exist (for example, on an unplugged SSD) are hidden, not deleted.
- Data lives in one SQLite file: `$GRACE_DATA_DIR`, else `$XDG_DATA_HOME/grace`, else `~/.local/share/grace/grace.db`.

Known tools: `claude`, `agy` / `antigravity`, `codex`, `gemini`, `cursor` / `cursor-agent`, `opencode`, `aider`, `copilot`, `amp`, `goose`, `crush`, `qwen`. To add one, edit `src/tools.rs`.

grace never tracks `/` or your home folder.

## Built with Claude

I built grace with [Claude Code](https://claude.com/claude-code). Claude Opus planned the design, wrote the shared skeleton and reviewed every part. Three Claude Sonnet agents wrote the modules in parallel: storage and ranking, the picker, and the shell integration. I gave the idea, the feature list and the final checks.

## License

MIT
