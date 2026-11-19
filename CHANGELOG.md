# Changelog

## 2.0.1

The program is the same as 2.0.0. This release is about install and docs.

- New: Homebrew tap. Run `brew install prathameshk421/grace/grace`. After the install, brew shows what grace does and how to set it up.
- Each release now updates the Homebrew formula on its own.
- README: a quick start at the top, real screenshots of the hub, `grace diff` and `grace tree`, Linux install details and uninstall steps.
- `scripts/screens.sh` makes the README screenshots from real grace runs on demo data.

## 2.0.0

- New: `grace hub`. It opens one terminal window on tmux. The sidebar is on the left, your shell is in the middle, and a live diff and a file tree are on the right.
- The sidebar shows live agents first, grouped by status, with the same glyphs as the picker. Your folders follow, best first. Type to filter.
- Each folder gets its own workspace, a tmux window with its own shell. Agents keep running when you switch.
- Enter in the sidebar switches to a folder. ctrl-o also starts the AI tool you used there last.
- Press ctrl-g, then `s`, `d`, `t` or `r` to show or hide the sidebar, diff, tree or whole right side. `z` zooms the shell. `g` goes to the sidebar. `q` detaches. `Q` quits the hub. `?` shows all keys.
- `grace hub quit` stops the hub from any terminal. After a detach, grace prints how to go back or stop the hub. The status bar shows the detach and quit keys.
- In the hub, `q` in the diff or tree pane hides that pane, and the layout remembers it.
- `grace diff` shows an untracked symlink and where it points. Control bytes in files show as `?`, so they cannot change colors or move the cursor.
- A pane that comes back takes its old place and size. grace saves the layout for new workspaces and the next start.
- The status bar shows each workspace with the status glyph of its agent.
- The hub uses its own tmux server (`tmux -L grace`) and its own config file. Your tmux setup stays as it is.
- The hub needs tmux 3.2 or newer. grace prints install hints when tmux is missing. On Windows, use WSL.
- New: `grace diff`. It shows the changed files of a git repository and a colored diff of the selected file. The header shows the branch and the commits ahead and behind.
- New: `grace tree`. It shows the files of a folder as a tree. It marks changed files and the folders that hold them. `o` opens a file.
- `grace diff` and `grace tree` refresh every 2 seconds. They fit in narrow panes.
- The zsh, bash, fish and PowerShell functions pass `hub`, `diff` and `tree` through to grace.

## 1.3.0

- Live sessions. The picker shows `● needs you`, `○ your turn`, `◐ working` or `✓ done` for CLI agents in each folder.
- `grace sessions` opens a picker with live agents only. Agents that need you come first. `grace sessions -l` prints a list.
- `grace setup claude` adds hooks to the Claude Code CLI settings. `grace setup codex` sets `notify` in the Codex CLI config. Both make a backup first, and `--remove` undoes them.
- `grace setup codex` keeps an existing Codex `notify` program and forwards each event to it. `--remove` restores it.
- `grace setup` also supports Gemini CLI, Qwen Code, GitHub Copilot CLI, OpenCode, Antigravity CLI, Crush, Goose, Amp and Aider. Each one gets the statuses its CLI reports.
- zsh and fish mark an AI CLI as working when it starts and done when it exits.
- grace clears sessions whose terminal closed.
- New picker design. It opens in a box under your prompt, not in full screen. `grace --full` gives the full-screen picker.
- The picker shows columns for status, name, parent folder, tags and time. A spinner shows agents that are working.
- Tags use the color of each tool's logo, for example Claude orange and VS Code blue.
- Open editor windows. The picker marks folders that VS Code, Cursor, Windsurf or Antigravity has open with `▣`. ctrl-e brings that window to the front.
- ctrl-o in the picker travels and starts the last AI tool used in that folder.
- grace imports editor history again every 10 minutes, so new folders show without `grace import`.

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
