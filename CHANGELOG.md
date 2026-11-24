# Changelog

## 2.2.0

### Performance & Binary Slimming

- **37% smaller release binary**: trimmed unused `clap`/`ratatui` features and enabled LTO + `opt-level = "s"` + symbol stripping (`codegen-units = 1`, `panic = "abort"`), cutting the macOS arm64 binary from `4.1M` to `2.6M`.
- **Faster CLI startup & hooks**: made `Config`, SQLite `Db`, tool `Registry`, and `IgnoreMatcher` loading lazy (`grace init` opens SQLite read-only when present, `add`/`tag`/`session end`/agent hooks skip building `Registry` via `db.normalize_tool_tag`, and Zsh/Fish AI hooks coalesce `tag` + `session start` into a single `grace session start --tag` call).
- **Snappier Picker, Diff, Tree & Hub**:
  - Moved live open-editor scanning (`editors_live::open_editors`) into `after_first_frame` so fast-travel queries skip editor scanning completely and background reloads skip re-querying entries when nothing new was imported.
  - Added dirty-flag TUI render loops ignoring mouse move noise, cached `Sidebar` data across hub-window-only updates, async startup + debounced diff loading in `grace diff`, and non-numstat git status + early directory skipping in `grace tree`.
  - Replaced DOM parsing of `~/.claude.json` with a zero-DOM iterative streaming JSON scanner and precompiled `IgnoreMatcher` + O(N) ranking in `rank::sort_entries`.

### Reliability, Security & Shell Polish

- **`grace doctor` upgrades**: added `config.toml` key/type/enum validation (`theme`, `editor`, `ascii`, `hub_layout`, `[hub].layout`, `ignore`), `$ZDOTDIR`/`~/.profile`/`$XDG_CONFIG_HOME` shell detection, read-only DB inspection (`inspect_sessions`, `list_dir_flags`), `0600` permission checks on `grace.db`, `-wal`, `-shm`, and `hub.lock`, fixable-only `--fix` footer hints, and `socket_exists` fast-path for tmux checks.
- **Shell wrappers & completions**:
  - Fixed argument scanning before `--` across Zsh, Bash, Fish, and PowerShell wrappers (`-h`/`--help`/`-V`/`--version`, bundled `sessions -lo`, and `--` stop).
  - Fixed `~` / `~/*` expansion in Zsh & Fish (no longer corrupts `~user`), improved command/argument tokenization in Bash, Fish, and PowerShell, added `__GraceResolveExe` dynamic fallback in PowerShell, and aligned all four completion scripts (`-t`/`--tag`, `-o`/`--open`, `pick`, `tool add`, directory completions, and Fish parity).
- **CLI & installer fixes**: preserved terminal editor flags and POSIX/PowerShell quoting on launch, wired `config.toml` `editor` into `grace diff` and `grace tree`, guarded `grace pin` against missing/ignored folders and `grace setup --all <target>` conflicts, cleaned up staged binaries on trap failure in `install.sh`, and guarded `$PROFILE` creation in `install.ps1` so existing profiles are not overwritten.

## 2.1.0

### Highlights & UI/UX ("Site of Grace")

- **Unified adaptive theme**: Consistent Dark, Light, 256-color, `NO_COLOR`, and ASCII modes (`src/theme.rs`, auto-detecting `COLORTERM`, `WT_SESSION`, `Apple_Terminal`, `COLORFGBG`).
- **Interactive Picker overhaul**:
  - `Tab` / `Shift-Tab` switches live between **Folders** and **Sessions** tabs (or click tabs with the mouse).
  - `Alt-i` toggles an inline **Inspector** panel with git status/ahead-behind, tool counts, pinned state, and active session details.
  - Direct actions inside the picker: `Alt-p` pin/unpin, `Ctrl-d` forget (with confirmation), `Ctrl-y` copy path (native clipboard + OSC 52), `Ctrl-g` open in `grace hub`, `?` keybind modal (and `a` / `i` to add current folder or import history when empty).
  - Zero-subprocess git branch & worktree discovery integrated into picker search & Inspector (plus active/open `◆` / `◇` hub workspace badges in the hub sidebar).
- **Frameless Hub (`grace hub`)**:
  - Focus-dimmed active borders, live `@grace_meta` headers (changed file counts, branch status), clickable panes/rows, `l` to cycle layout presets (`full` / `code+diff` / `sidebar+main` / `zen`), `P` to restore saved pane widths, `i` in `grace tree` to paste path to main pane, `n`/`p`/`Tab`/`w`/`1–9` workspace keys, and clear `✦ CTRL-G` command-mode indicator.
- **Diff & Tree upgrades**:
  - `grace diff`: old/new file line numbers, subtle green/red line washes, hunk jumping (`]`/`[`), stage/unstage (`s`/`u`), diff scope toggle (`S`: `all` / `unstaged` / `staged`), search (`/`, `n`/`N`), clickable file list & diff scroll.
  - `grace tree`: crisp tree indent guides, `/` live filename filter, `c` changed-only toggle, `E`/`C` expand/collapse all, `y` copy path, `i` insert path into hub shell.
- **Branded CLI help**: Clean colorized CLI output (respecting `NO_COLOR`/`CLICOLOR`/`TERM=dumb`), `grace --full <cmd>` anywhere before `--`, and `grace -t claude` / `grace -o <query>` direct picker flags.

### New Commands & Configuration

- `grace doctor [--fix]` — checks binary/PATH, shell integration, database permissions & `PRAGMA quick_check`, all 11 agent hooks, tmux hub readiness, and editor stores (with `--fix` auto-repair).
- `grace prune [--dry-run] [--older-than DAYS]` — clean up tracked folders that no longer exist on disk.
- `grace stats` — usage dashboard across tracked folders, visits, AI tools, and live sessions.
- `grace setup` — shows a status overview board across all 11 CLI agents; add `--all` to configure (or remove) every detected agent in one command.
- `grace list --json` & `grace sessions --json` — machine-readable JSON output for scripts.
- `grace completions <shell>` — tab completions for `zsh`, `bash`, `fish`, and `powershell`.
- Optional config file (`~/.config/grace/config.toml`, `%APPDATA%\grace\config.toml`, or `$GRACE_CONFIG`) for `theme`, `editor`, `ignore` globs, `ascii`, and `hub_layout`.

### Security & Cross-Platform Reliability

- Owner-only database directory (`0700`) and SQLite file (`0600`) permissions on Unix, SQLite `SQLITE_DBCONFIG_DEFENSIVE` + `trusted_schema = OFF`, `BEGIN IMMEDIATE` write locking, schema migrations, and improved session matching by `session_id` and `tmux_pane`.
- Eliminated shell `eval` for GUI editor launching and `cmd /C start` on Windows (now uses argv-safe `Command` / Win32 `ShellExecuteW`), sanitized all display strings against ANSI/BiDi control injection, and rejected line breaks/NULs before stdout emission.
- Atomic `0600` symlink-safe config writes across all 11 agent integrations, user-filtered live editor detection, capped file/subprocess reads, and zero-eval Bash history tokenizer + Fish empty-Enter fix + PowerShell async prompt preserving `$?`/`$LASTEXITCODE`.
- Expanded release binaries to **8 targets**, adding static `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, and `aarch64-pc-windows-msvc` (Windows ARM64), alongside hardened `install.sh` / `install.ps1` with SHA-256 and archive-path validation.

## 2.0.2

- Fix: with Homebrew, `grace setup` saved the versioned Cellar path in the CLI hooks. The next `brew upgrade` deleted that folder, and Claude Code showed "SessionStart hook failed". grace now saves the stable `/opt/homebrew/bin/grace`. The hub's ctrl-g keys use it too. Run `grace setup <cli>` again to update an older setup.

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
