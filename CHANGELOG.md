# Changelog

## 1.3.0

- Live sessions. The picker shows `● needs you`, `◐ working` or `✓ done` for CLI agents in each folder.
- `grace sessions` opens a picker with live agents only. Agents that wait for you come first. `grace sessions -l` prints a list.
- `grace setup claude` adds hooks to the Claude Code CLI settings. `grace setup codex` sets `notify` in the Codex CLI config. Both make a backup first, and `--remove` undoes them.
- zsh and fish mark an AI CLI as working when it starts and done when it exits.
- grace clears sessions whose terminal closed.

## 1.2.0

- New README with a demo image, a logo and a feature tour.
- One-line installers: `install.sh` for macOS and Linux, `install.ps1` for Windows. Both check the SHA-256 of the download.
- `grace --help` shows examples. It says that `grace` alone opens the picker.
- `grace init --help` shows the setup line for each shell.
- The hook commands `add` and `tag` no longer show in the help list. They still work.
- CI runs both installers on macOS, Linux and Windows.

## 1.1.1

- Add T3 Code as a built-in tool. The command is `t3` and the tag is `t3code`.
- Import recent folders from T3 Code with `grace import t3code`. The first-run import includes it.
- The picker has a fixed color for the `t3code` tag.

## 1.1.0

- Add editors as built-in tools: VS Code, VS Code Insiders, VSCodium, Windsurf, Zed, Neovim, Vim, Sublime Text, IntelliJ IDEA, PyCharm and WebStorm.
- `grace tool list` shows the kind of each tool: ai, editor or custom.
- Tag the folder you pass to the tool. `code ~/other` in `~/a` now tags `~/other`. The command is now `grace tag <path> <tool> [target]`. All four shell hooks pass the target.
- Import recent folders from VS Code, VSCodium, Cursor, Windsurf, Antigravity and Zed with `grace import [source]`. grace reads editor data read-only.
- The first-run import now runs for all sources.
- The picker has fixed colors for the new tags.

## 1.0.0

- First release for macOS, Linux and Windows.
- Shell hooks for zsh, bash, fish and PowerShell.
- Tags for AI tools you run in a folder.
- Custom tools with `grace tool add`.
- Import of Claude Code history.
