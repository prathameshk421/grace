# Changelog

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
