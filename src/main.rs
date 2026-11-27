mod config;
mod db;
mod diffview;
mod doctor;
mod editors_live;
mod git;
mod hub;
mod import;
mod import_editors;
mod integrations;
mod launcher;
mod model;
mod picker;
mod platform;
mod rank;
mod session;
mod setup;
mod shell;
mod sidebar;
mod theme;
mod tools;
mod treeview;
mod tui;
mod widgets;

use anyhow::{bail, Result};
use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

fn cli_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .usage(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .literal(AnsiColor::Green.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Cyan.on_default())
        .error(AnsiColor::Red.on_default() | Effects::BOLD)
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Yellow.on_default())
}

const BEFORE_HELP: &str = "\
✦ grace — Site of Grace for your projects & coding agents";

const AFTER_HELP: &str = "\
Examples:
  grace                 open the picker
  grace knn             jump to the only match for \"knn\"
  grace -w              open picker in \"Open with...\" launcher mode
  grace -t claude       only folders where you used Claude
  grace -o              travel and start the AI tool used there last
  grace hub             open the hub: sidebar, shell, diff and tree (needs tmux)
  grace sessions        see which agent works or waits in which folder
  grace setup --all     connect detected CLI agents for live status
  grace doctor          diagnose hooks, database, shell integration, and tmux
  grace stats           view project & agent usage statistics
  grace prune           remove tracked folders that no longer exist
  grace init --help     print shell setup instructions";

const INIT_HELP: &str = "\
Add one line to the config file of your shell:

  zsh         ~/.zshrc
              eval \"$(grace init zsh)\"
  bash        ~/.bashrc
              eval \"$(grace init bash)\"
  fish        ~/.config/fish/config.fish
              grace init fish | source
  powershell  $PROFILE
              Invoke-Expression (& grace init powershell | Out-String)

Open a new terminal after you save the file (or run `grace doctor`).";

#[derive(Parser)]
#[command(
    name = "grace",
    version,
    styles = cli_styles(),
    before_help = BEFORE_HELP,
    about = "Fast travel between your project folders",
    long_about = "Fast travel between your project folders.\nRun `grace` with no command to open the picker.",
    after_help = AFTER_HELP,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// Draw the picker on the full screen, not below the prompt (or set GRACE_FULLSCREEN=1)
    #[arg(long, global = true)]
    full: bool,
    /// Show only folders with this tool tag, for example "claude"
    #[arg(short, long)]
    tag: Option<String>,
    /// Also print the launch command of the last tool used there
    #[arg(short, long)]
    open: bool,
    /// Open directly into the app/editor/CLI launcher ("Open with...")
    #[arg(short = 'w', long = "with")]
    with: bool,
    /// Start the picker with this search text
    query: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the picker and print the chosen path
    Pick {
        /// Show only folders with this tool tag, for example "claude"
        #[arg(short, long)]
        tag: Option<String>,
        /// Also print the launch command of the last tool used there
        #[arg(short, long)]
        open: bool,
        /// Open directly into the app/editor/CLI launcher ("Open with...")
        #[arg(short = 'w', long = "with")]
        with: bool,
        /// Start the picker with this search text
        query: Option<String>,
    },
    /// Record a visit to a folder (used by the shell hook)
    #[command(hide = true)]
    Add { path: PathBuf },
    /// Record that an AI tool ran in a folder (used by the shell hook)
    #[command(hide = true)]
    Tag {
        /// The folder where the command ran
        path: PathBuf,
        tool: String,
        /// Folder argument of the command, relative to <path>
        target: Option<String>,
    },
    /// Print tracked folders, best first
    List {
        /// Show only folders with this tool tag
        #[arg(short, long)]
        tag: Option<String>,
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Keep a folder at the top of the list
    Pin {
        /// Default: the current folder
        path: Option<PathBuf>,
    },
    /// Remove the pin from a folder
    Unpin {
        /// Default: the current folder
        path: Option<PathBuf>,
    },
    /// Remove a folder from grace
    Forget {
        /// Default: the current folder
        path: Option<PathBuf>,
    },
    /// Import folders from Claude Code and editor history
    Import {
        /// Where to import from
        #[arg(default_value = "all", value_parser = clap::builder::PossibleValuesParser::new(import_editors::SOURCES))]
        source: String,
    },
    /// Add, remove or list AI tools
    Tool {
        #[command(subcommand)]
        action: ToolCmd,
    },
    /// Show folders where a CLI agent runs, and whether it works or waits for you
    Sessions {
        /// Print a plain table instead of the picker
        #[arg(short, long)]
        list: bool,
        /// Output machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Also print the launch command of the last tool used there
        #[arg(short, long)]
        open: bool,
        /// Start the picker with this search text
        query: Option<String>,
    },
    /// Connect CLI agents so grace sees their live session status
    Setup {
        /// Agent name (e.g. claude, codex, gemini, agy). Omit to inspect all.
        target: Option<String>,
        /// Setup (or --remove) all detected CLI agents at once
        #[arg(long, conflicts_with = "target")]
        all: bool,
        /// Undo the setup
        #[arg(long)]
        remove: bool,
    },
    /// Diagnose shell integration, database health, agent hooks, and tmux
    Doctor {
        /// Repair safe issues (DB permissions, outdated agent hooks, stale sessions)
        #[arg(long)]
        fix: bool,
    },
    /// Remove tracked folders that no longer exist on disk
    Prune {
        /// Preview what would be removed without modifying the database
        #[arg(long)]
        dry_run: bool,
        /// Only prune folders not visited in at least N days
        #[arg(long, value_name = "DAYS")]
        older_than: Option<i64>,
    },
    /// Show usage statistics across tracked folders and AI tools
    Stats,
    /// Print tab-completion script for a shell
    Completions {
        /// zsh, bash, fish, powershell (or pwsh)
        shell: String,
    },
    /// Live session tracking (used by shell and agent hooks)
    #[command(hide = true)]
    Session {
        #[command(subcommand)]
        action: SessionCmd,
    },
    /// Open the hub: a sidebar, your shell, a live diff and a file tree in one tmux window
    #[command(args_conflicts_with_subcommands = true, after_help = HUB_HELP)]
    Hub {
        #[command(subcommand)]
        action: Option<HubCmd>,
        /// Folder for the workspace. Default: the current folder
        path: Option<PathBuf>,
        /// Make the hub, but do not attach to it
        #[arg(long, hide = true)]
        no_attach: bool,
        /// Start this tool inside the workspace (e.g. "claude", "opencode")
        #[arg(long = "run-tool", value_name = "TOOL")]
        run_tool: Option<String>,
    },
    /// Show the git changes of a folder, with a colored diff
    Diff {
        /// Default: the current folder
        path: Option<PathBuf>,
    },
    /// Show the files of a folder as a tree, with git changes marked
    Tree {
        /// Default: the current folder
        path: Option<PathBuf>,
    },
    /// Print the shell setup code
    #[command(after_long_help = INIT_HELP, after_help = INIT_HELP)]
    Init {
        /// zsh, bash, fish, powershell (or pwsh)
        shell: String,
    },
}

#[derive(Subcommand)]
enum HubCmd {
    /// Stop the hub: close every workspace and the shells in them
    Quit,
    /// Show or hide a pane: sidebar, diff, tree, right, zoom or focus
    #[command(hide = true)]
    Toggle {
        what: String,
        /// tmux window id. Default: the window of this pane
        window: Option<String>,
    },
    /// The sidebar pane
    #[command(hide = true)]
    Sidebar { path: PathBuf },
    /// The main pane: runs your shell
    #[command(hide = true)]
    Shell {
        /// Optional command to launch immediately (for hub action resume)
        #[arg(long)]
        run: Option<String>,
        /// Optional tool tag to resolve and launch immediately
        #[arg(long = "run-tool", value_name = "TOOL")]
        run_tool: Option<String>,
    },
    /// Print the keys of the hub
    #[command(hide = true)]
    Keys {
        /// Wait for a key before exit
        #[arg(long)]
        wait: bool,
    },
    /// Ring the hub doorbell so idle panes refresh on state transitions
    #[command(hide = true)]
    Wake,
}

#[derive(Subcommand)]
enum SessionCmd {
    /// A CLI agent starts in a folder
    Start {
        path: PathBuf,
        tool: String,
        /// PID of the shell that runs the agent
        #[arg(long)]
        pid: Option<i64>,
        /// Also record the tool run for this folder
        #[arg(long)]
        tag: bool,
        /// Folder argument of the command, relative to <path>
        #[arg(long)]
        target: Option<String>,
    },
    /// The shell finished the agent command
    End {
        #[arg(long)]
        pid: i64,
    },
    /// Set the status of a session
    Set {
        path: PathBuf,
        tool: String,
        /// working, waiting, ready or done
        status: String,
    },
    /// Read a hook event of an agent CLI. Never fails, prints nothing.
    Hook {
        #[command(subcommand)]
        agent: HookCmd,
    },
}

#[derive(Subcommand)]
enum HookCmd {
    /// Claude Code CLI: the JSON comes on stdin
    Claude,
    /// Codex CLI notify: the JSON is the argument
    /// Args: [--then PROG ARGS...] JSON. The last arg is the JSON.
    Codex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Other CLIs (see src/integrations): NAME ARGS...
    #[command(external_subcommand)]
    Other(Vec<String>),
}

#[derive(Subcommand)]
enum ToolCmd {
    /// Register a command as an AI tool, for example `grace tool add kiro`
    Add {
        /// The command name you type in the shell
        command: String,
        /// Tag to show for this tool. Default: the command in lowercase
        #[arg(long)]
        tag: Option<String>,
        /// Command that starts the tool. Default: the command
        #[arg(long)]
        launch: Option<String>,
    },
    /// Remove a custom tool (built-in tools stay)
    Rm { command: String },
    /// Print all tools: command, tag, launch command, kind
    List,
}

const HUB_HELP: &str = "\
The hub uses tmux on its own server, so your tmux setup stays as it is.

Keys: press ctrl-g, then
  s  sidebar      d  diff         t  tree
  r  right side   l  cycle layout P  fit widths
  z  zoom main    g  go to sidebar
  n  next ws      p  prev ws      Tab last ws
  w  close ws     q  detach       Q  quit hub
  1-9 jump ws     ?  all keys

Leave the hub:
  ctrl-g q         detach. The hub and your agents keep running.
                   Run `grace hub` to go back.
  ctrl-g Q         quit. Closes every workspace and stops the hub.
  grace hub quit   the same, from any terminal.
  exit             in a workspace shell, closes that workspace.

In the sidebar, type to filter. Enter opens the folder in its own workspace.
ctrl-o also starts the AI tool you used there last.";

const NEW_TERMINAL: &str = "Open a new terminal (or restart your shell) to start tracking it.";

fn resolve(path: Option<PathBuf>) -> Result<String> {
    let p = match path {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    Ok(platform::normalize(&p))
}

fn current_tmux_pane() -> Option<String> {
    std::env::var_os("TMUX")?;
    std::env::var("TMUX_PANE").ok().filter(|s| !s.is_empty())
}

pub(crate) fn load(
    db: &db::Db,
    reg: &tools::Registry,
    ignore: &platform::IgnoreMatcher,
    tag: Option<&str>,
) -> Result<Vec<model::Entry>> {
    let now = model::now();
    let norm_tag = tag.map(|t| reg.normalize_tag(t));
    let mut entries: Vec<_> = db
        .list_entries()?
        .into_iter()
        .filter(|e| norm_tag.as_deref().is_none_or(|t| e.has_tag(t)))
        .filter(|e| !ignore.is_ignored(&e.path) && Path::new(&e.path).is_dir())
        .collect();
    rank::sort_entries(&mut entries, now);
    Ok(entries)
}

fn collect_branches(entries: &[model::Entry]) -> std::collections::HashMap<String, (String, bool)> {
    let mut out = std::collections::HashMap::new();
    for e in entries.iter().take(40) {
        if let Some(br) = git::fast_branch(Path::new(&e.path)) {
            out.insert(e.path.clone(), br);
        }
    }
    out
}

/// Session commands: no first-run import, so they stay fast.
fn run_session(action: SessionCmd) -> Result<()> {
    match action {
        SessionCmd::Hook { agent } => {
            // A hook must never fail loudly and never print to stdout.
            let _ = run_hook(agent);
            return Ok(());
        }
        SessionCmd::End { pid } => {
            let (db, _) = db::Db::open_default()?;
            db.session_end_pid(pid, model::now())?;
            hub::sync_glyphs_if_running(&db, None);
            return Ok(());
        }
        SessionCmd::Start {
            path,
            tool,
            pid,
            tag,
            target,
        } => {
            let cfg = config::load();
            let ignore = platform::IgnoreMatcher::new(&cfg.ignore);
            let (db, _) = db::Db::open_default()?;
            let reg = tools::Registry::from_db(&db)?;
            let now = model::now();
            let pane = current_tmux_pane();
            let tool = reg.normalize_tag(&tool);
            let cwd = resolve(Some(path))?;
            let p = platform::tag_target(&cwd, target.as_deref());
            if reg.is_ai_tag(&tool) && !ignore.is_ignored(&p) {
                if tag {
                    db.record_tool(&p, &tool, now)?;
                }
                db.session_start(
                    &p,
                    &tool,
                    pid,
                    db::SessionMeta {
                        tmux_pane: pane.as_deref(),
                        ..Default::default()
                    },
                    now,
                )?;
                hub::sync_glyphs_if_running(&db, Some(&cfg));
            }
        }
        SessionCmd::Set { path, tool, status } => {
            let Some(status) = model::Status::parse(&status) else {
                bail!("status must be working, waiting, ready or done");
            };
            let cfg = config::load();
            let ignore = platform::IgnoreMatcher::new(&cfg.ignore);
            let (db, _) = db::Db::open_default()?;
            let reg = tools::Registry::from_db(&db)?;
            let now = model::now();
            let pane = current_tmux_pane();
            let tool = reg.normalize_tag(&tool);
            let p = resolve(Some(path))?;
            if reg.is_ai_tag(&tool) && !ignore.is_ignored(&p) {
                let outcome = db.session_set(
                    &p,
                    &tool,
                    status,
                    db::SessionMeta {
                        tmux_pane: pane.as_deref(),
                        ..Default::default()
                    },
                    now,
                )?;
                if outcome.inserted || outcome.prev != Some(status) {
                    hub::sync_glyphs_if_running(&db, Some(&cfg));
                }
            }
        }
    }
    Ok(())
}

fn run_hook(agent: HookCmd) -> Result<()> {
    use std::io::Read;
    let (tool, status, cwd, sid, detail) = match agent {
        HookCmd::Claude => {
            let mut input = String::new();
            std::io::stdin().take(1 << 20).read_to_string(&mut input)?;
            let Some(ev) = session::parse_claude_event(&input) else {
                return Ok(());
            };
            ("claude", ev.status, ev.cwd, ev.session_id, ev.detail)
        }
        HookCmd::Codex { args } => {
            let (json, chained) = session::split_codex_args(&args);
            // Own update first, then forward. Forward even when our update fails.
            let own = json.and_then(session::parse_codex_event).map(|(cwd, sid)| {
                set_hook_status("codex", model::Status::Ready, cwd, sid.as_deref(), None)
            });
            if let (Some(json), Some(chained)) = (json, chained) {
                forward_codex(chained, json);
            }
            return own.unwrap_or(Ok(()));
        }
        HookCmd::Other(argv) => {
            let Some(integ) = argv.first().and_then(|n| integrations::find(n)) else {
                return Ok(());
            };
            let mut input = String::new();
            if integ.reads_stdin && !std::io::stdin().is_terminal() {
                let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
            }
            let res = (integ.hook)(&argv[1..], &input);
            // The CLI may parse our stdout: print what it expects, even if our update fails.
            if !integ.stdout.is_empty() {
                println!("{}", integ.stdout);
            }
            let Some(status) = res.status else {
                return Ok(());
            };
            (integ.tool, status, res.cwd, res.session_id, res.detail)
        }
    };
    set_hook_status(tool, status, cwd, sid.as_deref(), detail.as_deref())
}

/// Run the chained notify program with the same JSON, detached. Errors are ignored.
fn forward_codex(chained: &[String], json: &str) {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(&chained[0]);
    cmd.args(&chained[1..])
        .arg(json)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    platform::apply_no_window(&mut cmd);
    let _ = cmd.spawn();
}

fn set_hook_status(
    tool: &str,
    status: model::Status,
    cwd: Option<String>,
    session_id: Option<&str>,
    detail: Option<&str>,
) -> Result<()> {
    let cfg = config::load();
    let ignore = platform::IgnoreMatcher::new(&cfg.ignore);
    let p = resolve(cwd.map(PathBuf::from))?;
    if ignore.is_ignored(&p) {
        return Ok(());
    }
    let (db, _) = db::Db::open_default()?;
    let tool = db.normalize_tool_tag(tool);
    let pane = current_tmux_pane();
    let agent = platform::agent_pin();
    let sid = session_id.filter(|s| !s.is_empty());
    let det = detail
        .map(platform::sanitize_display)
        .filter(|s| !s.is_empty());
    let outcome = db.session_set(
        &p,
        &tool,
        status,
        db::SessionMeta {
            session_id: sid,
            detail: det.as_deref(),
            tmux_pane: pane.as_deref(),
            agent,
        },
        model::now(),
    )?;
    if outcome.inserted || outcome.prev != Some(status) {
        hub::sync_glyphs_if_running(&db, Some(&cfg));
    }
    Ok(())
}

/// Build an argv-safe editor choice for `ctrl-e`. Never uses shell evaluation for GUIs.
fn editor_choice(
    installed: Option<&[launcher::InstalledTarget]>,
    reg: &tools::Registry,
    cfg: &config::Config,
    e: &model::Entry,
    open: Option<&editors_live::OpenEditor>,
) -> Option<picker::EditorChoice> {
    launcher::resolve_editor_for_entry(installed, reg, cfg, e, open)
}

fn editor_label(
    reg: &tools::Registry,
    cfg: &config::Config,
    e: &model::Entry,
    open: Option<&editors_live::OpenEditor>,
) -> Option<String> {
    if let Some(o) = open {
        return Some(o.tag.clone());
    }
    if let Some(tag) = e.tags.iter().find(|t| {
        reg.is_editor_tag(&t.tool)
            || matches!(t.tool.as_str(), "t3code" | "claude-desktop" | "codex-app")
    }) {
        return Some(tag.tool.clone());
    }
    cfg.editor
        .as_deref()
        .and_then(|s| s.split_whitespace().next())
        .map(String::from)
}

fn resume_tag<'a>(reg: &tools::Registry, e: &'a model::Entry) -> Option<&'a str> {
    e.tags
        .iter()
        .find(|t| reg.is_ai_tag(&t.tool))
        .map(|t| t.tool.as_str())
        .or_else(|| e.last_tool())
}

/// Folders with a live session, most urgent first, and the sessions.
fn session_data(
    db: &db::Db,
    ignore: &platform::IgnoreMatcher,
    now: i64,
) -> Result<(Vec<model::Entry>, Vec<model::Session>)> {
    let mut sessions = db.sessions(now)?;
    session::sort_sessions(&mut sessions);
    let best = picker::best_by_path(&sessions);
    if best.is_empty() {
        return Ok((Vec::new(), sessions));
    }
    let mut entries: Vec<_> = db
        .list_entries()?
        .into_iter()
        .filter(|e| {
            best.contains_key(&e.path) && !ignore.is_ignored(&e.path) && Path::new(&e.path).is_dir()
        })
        .collect();
    let key = |e: &model::Entry| {
        let b = &best[&e.path];
        (picker::urgency(b.status), b.updated_at)
    };
    entries.sort_by_key(|e| std::cmp::Reverse(key(e)));
    Ok((entries, sessions))
}

fn validate_emit_line(s: &str, label: &str) -> Result<()> {
    if platform::has_line_break_or_nul(s) {
        bail!("refusing to emit {label} containing control line breaks or NUL bytes");
    }
    Ok(())
}

pub(crate) fn spawn_gui_editor(ed: &picker::EditorChoice) -> Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(&ed.prog);
    cmd.args(&ed.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    platform::apply_no_window(&mut cmd);
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("cannot start editor `{}`: {e}", ed.prog))
}

fn detect_missing_shell_hook() -> Option<String> {
    if std::env::var_os("GRACE_SHELL").is_some() || !std::io::stdout().is_terminal() {
        return None;
    }
    if cfg!(windows) {
        return Some("Invoke-Expression (& grace init powershell | Out-String)".into());
    }
    let sh = std::env::var("SHELL").unwrap_or_default();
    let name = Path::new(&sh)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("zsh");
    let line = match name {
        "bash" => r#"eval "$(grace init bash)""#,
        "fish" => "grace init fish | source",
        _ => r#"eval "$(grace init zsh)""#,
    };
    Some(line.into())
}

struct TravelOpts<'a> {
    db: &'a db::Db,
    reg: &'a tools::Registry,
    cfg: &'a config::Config,
    ignore: &'a platform::IgnoreMatcher,
    view: picker::View,
    tag: Option<&'a str>,
    query: &'a str,
    open: bool,
    with: bool,
    full: bool,
    initial_toast: Option<(String, theme::ToastKind)>,
}

/// Run the picker and print the result for the shell function.
/// Line 1 is the folder to cd to (empty: stay). Line 2, if present, is a static command to run.
fn travel(t: TravelOpts<'_>, data: picker::Data) -> Result<()> {
    let TravelOpts {
        db,
        reg,
        cfg,
        ignore,
        view,
        tag,
        query,
        open,
        with,
        full,
        initial_toast,
    } = t;
    let entries_snap = data.entries.clone();
    let sessions_snap = data.sessions.clone();
    let init_ver = db.data_version();
    let init_deadline = db::Db::next_deadline(&data.sessions);
    let init_has_pinned = data.sessions.iter().any(|s| {
        s.status != model::Status::Done && (s.shell_pid.is_some() || s.agent_pid.is_some())
    });
    let tag_owned = tag.map(String::from);
    let reload_tag = tag_owned.clone();
    let mut last_view = view;
    let mut last_ver = init_ver;
    let mut next_deadline = init_deadline;
    let mut has_pinned_open = init_has_pinned;
    let mut last_pin_check = std::time::Instant::now();
    let mut cached_editors = Vec::new();
    let mut editors_loaded = false;
    let mut last_editors_at = std::time::Instant::now();
    let reload = move |cur_view: picker::View| {
        let now = model::now();
        let now_inst = std::time::Instant::now();
        let switched_view = cur_view != last_view;
        let db_changed = db.data_version() != last_ver;
        let deadline_hit = now >= next_deadline;
        let pin_due =
            has_pinned_open && last_pin_check.elapsed() >= std::time::Duration::from_secs(10);
        let editors_periodic = last_editors_at.elapsed() >= std::time::Duration::from_secs(30);

        if cur_view == picker::View::Sessions
            && !switched_view
            && !db_changed
            && !deadline_hit
            && !pin_due
            && !editors_periodic
        {
            return None;
        }

        last_view = cur_view;
        let (entries, sessions) = match cur_view {
            picker::View::Folders => (
                load(db, reg, ignore, reload_tag.as_deref()).ok()?,
                db.sessions(now).unwrap_or_default(),
            ),
            picker::View::Sessions => session_data(db, ignore, now).ok()?,
        };
        if pin_due || switched_view {
            last_pin_check = now_inst;
        }
        next_deadline = db::Db::next_deadline(&sessions);
        has_pinned_open = sessions.iter().any(|s| {
            s.status != model::Status::Done && (s.shell_pid.is_some() || s.agent_pid.is_some())
        });
        last_ver = db.data_version();
        if !editors_loaded || editors_periodic {
            cached_editors = editors_live::open_editors(ignore);
            editors_loaded = true;
            last_editors_at = now_inst;
        }
        let branches = collect_branches(&entries);
        Some(picker::Data {
            entries,
            sessions,
            editors: cached_editors.clone(),
            branches,
            installed: None,
        })
    };
    let bg_tag = tag_owned;
    let bg_ignore = ignore.clone();
    let bg_cfg = cfg.clone();
    let bg_custom = db.custom_tools().unwrap_or_default();
    let after_first_frame: Option<Box<dyn FnOnce() -> Option<picker::Data> + Send + 'static>> =
        Some(Box::new(move || {
            let branches = collect_branches(&entries_snap);
            let editors = editors_live::open_editors(&bg_ignore);
            let mut installed = None;
            let mut entries = entries_snap;
            let mut sessions = sessions_snap;
            if let Ok((bg_db, _)) = db::Db::open_default() {
                if editors_live::refresh_if_stale_with(&bg_db, Some(&editors), &bg_ignore) {
                    let now = model::now();
                    if let Ok(bg_reg) = tools::Registry::from_db(&bg_db) {
                        match view {
                            picker::View::Folders => {
                                if let Ok(reloaded) =
                                    load(&bg_db, &bg_reg, &bg_ignore, bg_tag.as_deref())
                                {
                                    entries = reloaded;
                                }
                                sessions = bg_db.sessions(now).unwrap_or(sessions);
                            }
                            picker::View::Sessions => {
                                if let Ok((reloaded, s)) = session_data(&bg_db, &bg_ignore, now) {
                                    entries = reloaded;
                                    sessions = s;
                                }
                            }
                        }
                        installed = Some(launcher::discover_installed(&bg_reg, &bg_cfg));
                    }
                } else if let Ok(bg_reg) = tools::Registry::from_db(&bg_db) {
                    installed = Some(launcher::discover_installed(&bg_reg, &bg_cfg));
                }
            }
            if installed.is_none() {
                let fallback_reg = tools::Registry::new(&bg_custom);
                installed = Some(launcher::discover_installed(&fallback_reg, &bg_cfg));
            }
            Some(picker::Data {
                entries,
                sessions,
                editors,
                branches,
                installed,
            })
        }));

    let toggle_pin = Some(Box::new(
        move |e: &model::Entry| -> std::result::Result<bool, String> {
            let next = !e.pinned;
            db.set_pinned(&e.path, next)
                .map_err(|err| err.to_string())?;
            Ok(next)
        },
    )
        as Box<dyn FnMut(&model::Entry) -> std::result::Result<bool, String>>);

    let forget_dir = Some(
        Box::new(move |e: &model::Entry| -> std::result::Result<(), String> {
            db.forget(&e.path)
                .map(|_| ())
                .map_err(|err| err.to_string())
        }) as Box<dyn FnMut(&model::Entry) -> std::result::Result<(), String>>,
    );

    let import_all_now = Some(Box::new(move || -> std::result::Result<String, String> {
        let count: usize = import_editors::import_all(db, ignore)
            .into_iter()
            .map(|(_, n)| n)
            .sum();
        editors_live::mark_imported_now(db);
        Ok(format!("Imported {count} folder entries"))
    })
        as Box<dyn FnMut() -> std::result::Result<String, String>>);

    let add_cwd_now = Some(Box::new(move || -> std::result::Result<String, String> {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let norm = platform::normalize(&cwd);
        if ignore.is_ignored(&norm) {
            return Err("Current folder matches an ignore rule".into());
        }
        db.record_visit(&norm, model::now())
            .map_err(|e| e.to_string())?;
        Ok(format!("Added {}", platform::sanitize_display(&norm)))
    })
        as Box<dyn FnMut() -> std::result::Result<String, String>>);

    let has_tmux = launcher::has_tmux();
    let cli_in_hub = cfg.cli_in_hub.unwrap_or(true) && has_tmux;
    let default_launch = with
        || cfg
            .default_action
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("launch"));
    let discover_launchers = Some(Box::new(|| launcher::discover_installed(reg, cfg))
        as Box<dyn FnMut() -> Vec<launcher::InstalledTarget>>);

    let opts = picker::Opts {
        view,
        fullscreen: full,
        start_in_launch: with,
        default_launch,
        cli_in_hub,
        has_tmux,
        tag_filter: tag.map(String::from),
        initial_toast,
        shell_hook_missing: detect_missing_shell_hook(),
        editor_cmd: Box::new(|e, o, inst| editor_choice(inst, reg, cfg, e, o)),
        tool_label: Box::new(|e| resume_tag(reg, e).map(String::from)),
        editor_label: Box::new(|e, o| editor_label(reg, cfg, e, o)),
        toggle_pin,
        forget_dir,
        import_all_now,
        add_cwd_now,
        discover_launchers,
        after_first_frame,
        reload: Some(Box::new(reload)),
        reload_every: Some(Box::new(|v| match v {
            picker::View::Folders => None,
            picker::View::Sessions => Some(std::time::Duration::from_secs(2)),
        })),
    };
    let Some(choice) = picker::pick(data, query, model::now(), cfg, opts)? else {
        std::process::exit(1);
    };
    emit_choice(Some(db), ignore, reg, cli_in_hub, &choice, open)
}

pub(crate) fn emit_editor_cmd(ed: &picker::EditorChoice) -> Result<String> {
    validate_emit_line(&ed.prog, "editor command")?;
    for a in &ed.args {
        validate_emit_line(a, "editor argument")?;
    }
    let parts = std::iter::once(&ed.prog)
        .chain(ed.args.iter())
        .map(|s| {
            if cfg!(windows) {
                platform::quote_powershell(s)
            } else {
                platform::quote_posix(s)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if cfg!(windows) {
        Ok(format!("& {parts}"))
    } else {
        Ok(parts)
    }
}

fn format_launch_lines(
    reg: &tools::Registry,
    cli_in_hub: bool,
    entry: &model::Entry,
    lc: &launcher::LaunchChoice,
) -> Result<(String, Option<String>)> {
    validate_emit_line(&entry.path, "path")?;
    match &lc.target.method {
        launcher::LaunchMethod::TerminalEditor { .. } => {
            let ed = launcher::build_editor_choice(&lc.target, &entry.path, None)?;
            let cmd = emit_editor_cmd(&ed)?;
            Ok((entry.path.clone(), Some(cmd)))
        }
        launcher::LaunchMethod::CliAgent {
            tool_tag,
            launch_cmd,
        } => {
            let canonical = reg.normalize_tag(tool_tag);
            let cmd = reg
                .launch_command(&canonical)
                .unwrap_or_else(|| launch_cmd.clone());
            if !tools::valid_launch(&cmd) {
                bail!("invalid launch command for `{canonical}`");
            }
            if !lc.force_shell && cli_in_hub && tools::valid_name(&canonical) {
                Ok((
                    entry.path.clone(),
                    Some(format!("grace hub --run-tool {canonical}")),
                ))
            } else {
                validate_emit_line(&cmd, "launch command")?;
                Ok((entry.path.clone(), Some(cmd)))
            }
        }
        launcher::LaunchMethod::HubShell => Ok((entry.path.clone(), Some("grace hub".to_string()))),
        launcher::LaunchMethod::Travel => Ok((entry.path.clone(), None)),
        launcher::LaunchMethod::Gui { .. } | launcher::LaunchMethod::GuiUrl { .. } => {
            bail!("GUI target does not emit shell lines")
        }
    }
}

fn emit_choice(
    db: Option<&db::Db>,
    ignore: &platform::IgnoreMatcher,
    reg: &tools::Registry,
    cli_in_hub: bool,
    choice: &picker::Choice,
    open: bool,
) -> Result<()> {
    let e = &choice.entry;
    let record = |tag: &str| {
        if let Some(d) = db {
            if !ignore.is_ignored(&e.path) && !tag.is_empty() {
                let _ = d.record_tool(&e.path, tag, model::now());
            }
        }
    };
    match &choice.action {
        picker::Action::Editor(ed) => {
            if ed.in_terminal {
                validate_emit_line(&e.path, "path")?;
                let cmd = emit_editor_cmd(ed)?;
                println!("{}", e.path);
                println!("{cmd}");
            } else {
                spawn_gui_editor(ed)?;
            }
        }
        picker::Action::Hub => {
            validate_emit_line(&e.path, "path")?;
            println!("{}", e.path);
            println!("grace hub");
        }
        picker::Action::Travel | picker::Action::Resume => {
            validate_emit_line(&e.path, "path")?;
            println!("{}", e.path);
            if open || choice.action == picker::Action::Resume {
                if let Some(cmd) = resume_tag(reg, e).and_then(|t| reg.launch_command(t)) {
                    validate_emit_line(&cmd, "launch command")?;
                    println!("{cmd}");
                }
            }
        }
        picker::Action::Launch(lc) => match &lc.target.method {
            launcher::LaunchMethod::Gui { .. } | launcher::LaunchMethod::GuiUrl { .. } => {
                let open_list = editors_live::open_editors(ignore);
                let open_hit = launcher::find_open_for_target(&lc.target, &e.path, &open_list);
                let ed = launcher::build_editor_choice(&lc.target, &e.path, open_hit)?;
                record(&lc.target.tag);
                spawn_gui_editor(&ed)?;
            }
            launcher::LaunchMethod::CliAgent { tool_tag, .. } => {
                let canonical = reg.normalize_tag(tool_tag);
                let (dir, cmd) = format_launch_lines(reg, cli_in_hub, e, lc)?;
                record(&canonical);
                println!("{dir}");
                if let Some(c) = cmd {
                    println!("{c}");
                }
            }
            launcher::LaunchMethod::TerminalEditor { .. } => {
                let (dir, cmd) = format_launch_lines(reg, cli_in_hub, e, lc)?;
                record(&lc.target.tag);
                println!("{dir}");
                if let Some(c) = cmd {
                    println!("{c}");
                }
            }
            launcher::LaunchMethod::HubShell | launcher::LaunchMethod::Travel => {
                let (dir, cmd) = format_launch_lines(reg, cli_in_hub, e, lc)?;
                println!("{dir}");
                if let Some(c) = cmd {
                    println!("{c}");
                }
            }
        },
    }
    Ok(())
}

fn first_run_import_if_needed(
    db: &db::Db,
    ignore: &platform::IgnoreMatcher,
    fresh: bool,
) -> Option<(String, theme::ToastKind)> {
    if !fresh {
        return None;
    }
    let _ = import::import_claude(db, ignore);
    let _ = import_editors::import_editors_quiet(db, ignore);
    editors_live::mark_imported_now(db);
    let n = db.count_dirs().unwrap_or(0);
    if n > 0 {
        Some((
            format!("Welcome to grace — discovered {n} folder(s)"),
            theme::ToastKind::Info,
        ))
    } else {
        None
    }
}

fn print_stats(cfg: &config::Config, stats: &model::Stats, home: Option<&str>) {
    let c = theme::Ansi::stdout(cfg);
    println!();
    println!(
        "  {}{} grace stats{} {}— tracked workspace overview{}",
        c.gold, c.star, c.reset, c.dim, c.reset
    );
    println!();
    println!(
        "  {}Folders{}    {} total  {}·{} {} on disk  {}·{} {} missing  {}·{} {} pinned",
        c.bold,
        c.reset,
        stats.total_dirs,
        c.dim,
        c.reset,
        stats.existing_dirs,
        c.dim,
        c.reset,
        stats.missing_dirs,
        c.dim,
        c.reset,
        stats.pinned_dirs
    );
    println!(
        "  {}Activity{}   {} visits  {}·{} {} tool runs  {}·{} {} live session(s)",
        c.bold,
        c.reset,
        stats.total_visits,
        c.dim,
        c.reset,
        stats.total_tags,
        c.dim,
        c.reset,
        stats.live_sessions
    );
    if !stats.top_tools.is_empty() {
        println!();
        println!("  {}Top tools{}", c.bold, c.reset);
        let max_runs = stats.top_tools.first().map(|t| t.1).unwrap_or(1).max(1);
        for (tool, runs, folders) in stats.top_tools.iter().take(8) {
            let bar_len = ((*runs * 18) / max_runs).max(1) as usize;
            let bar = "█".repeat(bar_len);
            let tool_shown = platform::sanitize_display(tool);
            println!(
                "    {}{:<14}{} {}{:<18}{} {:>5} run{:<1} {}({} folder{}){}",
                c.cyan,
                tool_shown,
                c.reset,
                c.gold,
                bar,
                c.reset,
                runs,
                platform::plural(*runs),
                c.dim,
                folders,
                platform::plural(*folders),
                c.reset
            );
        }
    }
    if !stats.top_dirs.is_empty() {
        println!();
        println!("  {}Top folders{}", c.bold, c.reset);
        for e in &stats.top_dirs {
            let pin = if e.pinned { "★ " } else { "  " };
            let disp = picker::display_path(&e.path, home);
            let shown = platform::sanitize_display(&disp);
            println!(
                "    {}{}{} {:<44} {}{} visit{}{}",
                c.gold,
                pin,
                c.reset,
                shown,
                c.dim,
                e.visits,
                platform::plural(e.visits),
                c.reset
            );
        }
    }
    println!();
}

fn parse_cli_from<I, T>(args: I) -> std::result::Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let mut saw_full = false;
    let mut after_sep = false;
    let filtered: Vec<std::ffi::OsString> = args
        .into_iter()
        .enumerate()
        .filter_map(|(idx, a)| {
            let s: std::ffi::OsString = a.into();
            if idx == 0 {
                return Some(s);
            }
            if !after_sep && s == "--" {
                after_sep = true;
                return Some(s);
            }
            if !after_sep && s == "--full" {
                saw_full = true;
                return None;
            }
            Some(s)
        })
        .collect();
    let mut cli = Cli::try_parse_from(filtered)?;
    cli.full |= saw_full;
    Ok(cli)
}

fn run() -> Result<()> {
    let cli = parse_cli_from(std::env::args_os()).unwrap_or_else(|e| e.exit());
    match cli.cmd {
        Some(Cmd::Completions { shell }) => {
            let Some(sh) = shell::Shell::parse(&shell) else {
                bail!("unsupported shell: {shell} (use zsh, bash, fish, powershell or pwsh)");
            };
            print!("{}", shell::completions(sh));
            return Ok(());
        }
        Some(Cmd::Init { shell }) => {
            let Some(sh) = shell::Shell::parse(&shell) else {
                bail!("unsupported shell: {shell} (use zsh, bash, fish, powershell or pwsh)");
            };
            let reg = match db::Db::open_readonly_default()? {
                Some(db) => tools::Registry::from_db(&db)?,
                None => tools::Registry::new(&[]),
            };
            print!(
                "{}",
                shell::init(sh, &reg.command_names(), &reg.ai_command_names())
            );
            return Ok(());
        }
        Some(Cmd::Setup {
            target,
            all,
            remove,
        }) => return setup::run(target.as_deref(), all, remove),
        Some(Cmd::Session { action }) => return run_session(action),
        Some(Cmd::Doctor { fix }) => return doctor::run(fix),
        Some(Cmd::Hub {
            action: Some(HubCmd::Shell { run: cmd, run_tool }),
            ..
        }) => {
            if let Some(tag) = run_tool {
                if !tools::valid_name(&tag) {
                    bail!("invalid tool name `{tag}`");
                }
                let reg = match db::Db::open_readonly_default()? {
                    Some(db) => tools::Registry::from_db(&db)?,
                    None => tools::Registry::new(&[]),
                };
                let canonical = reg.normalize_tag(&tag);
                let Some(tool_cmd) = reg.launch_command(&canonical) else {
                    bail!("unknown tool `{tag}`");
                };
                if !tools::valid_launch(&tool_cmd) || platform::has_line_break_or_nul(&tool_cmd) {
                    bail!("invalid launch command for `{canonical}`");
                }
                return hub::shell(Some(&tool_cmd));
            }
            return hub::shell(cmd.as_deref());
        }
        Some(Cmd::Hub {
            action: Some(HubCmd::Keys { wait }),
            ..
        }) => return hub::keys(wait),
        Some(Cmd::Hub {
            action: Some(HubCmd::Wake),
            ..
        }) => return hub::wake(),
        Some(Cmd::Hub {
            action: Some(HubCmd::Quit),
            ..
        }) => return hub::quit(),
        Some(Cmd::Diff { path }) => {
            let cfg = config::load();
            let p = resolve(path)?;
            let in_hub = hub::pane_in_hub(hub::Role::Diff.as_str());
            let th = theme::Theme::from_env_and_config(&cfg);
            tui::run(
                &mut diffview::DiffView::new(p.into(), th)
                    .in_hub(in_hub)
                    .with_editor(cfg.editor.clone()),
            )?;
            return hub::pane_closed(in_hub, hub::Role::Diff);
        }
        Some(Cmd::Tree { path }) => {
            let cfg = config::load();
            let p = resolve(path)?;
            let in_hub = hub::pane_in_hub(hub::Role::Tree.as_str());
            let th = theme::Theme::from_env_and_config(&cfg);
            tui::run(
                &mut treeview::TreeView::new(p.into(), th)
                    .in_hub(in_hub)
                    .with_editor(cfg.editor.clone()),
            )?;
            return hub::pane_closed(in_hub, hub::Role::Tree);
        }
        _ => {}
    }

    let full = cli.full || std::env::var("GRACE_FULLSCREEN").is_ok_and(|v| v == "1");
    let now = model::now();
    let cfg = config::load();
    let ignore = platform::IgnoreMatcher::new(&cfg.ignore);
    let (db, fresh) = db::Db::open_default()?;
    let home = platform::home_dir().map(|h| platform::clean_path_str(&h.to_string_lossy()));

    let default_pick = || Cmd::Pick {
        tag: cli.tag.clone(),
        open: cli.open,
        with: cli.with,
        query: cli.query.clone(),
    };

    match cli.cmd.unwrap_or_else(default_pick) {
        Cmd::Pick {
            tag,
            open,
            with,
            query,
        } => {
            let reg = tools::Registry::from_db(&db)?;
            let toast = first_run_import_if_needed(&db, &ignore, fresh);
            let entries = load(&db, &reg, &ignore, tag.as_deref())?;
            let data = picker::Data {
                entries,
                sessions: db.sessions(now).unwrap_or_default(),
                editors: Vec::new(),
                branches: std::collections::HashMap::new(),
                installed: None,
            };
            let q = query.as_deref().unwrap_or("");
            travel(
                TravelOpts {
                    db: &db,
                    reg: &reg,
                    cfg: &cfg,
                    ignore: &ignore,
                    view: picker::View::Folders,
                    tag: tag.as_deref(),
                    query: q,
                    open,
                    with,
                    full,
                    initial_toast: toast,
                },
                data,
            )?;
        }
        Cmd::Sessions {
            list,
            json,
            open,
            query,
        } => {
            if json {
                let mut sessions = db.sessions(now)?;
                session::sort_sessions(&mut sessions);
                let items: Vec<_> = sessions
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id,
                            "path": s.path,
                            "tool": s.tool,
                            "status": s.status.as_str(),
                            "status_label": s.status.label(),
                            "shell_pid": s.shell_pid,
                            "started_at": s.started_at,
                            "updated_at": s.updated_at,
                            "session_id": s.session_id,
                            "detail": s.detail,
                            "tmux_pane": s.tmux_pane,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            if list {
                let c = theme::Ansi::stdout(&cfg);
                let mut sessions = db.sessions(now)?;
                session::sort_sessions(&mut sessions);
                for s in &sessions {
                    let st_col = match s.status {
                        model::Status::Waiting => c.yellow,
                        model::Status::Working => c.cyan,
                        model::Status::Ready | model::Status::Done => c.green,
                    };
                    let det = s
                        .detail
                        .as_deref()
                        .map(platform::sanitize_display)
                        .filter(|d| !d.is_empty())
                        .map(|d| format!(" {}({d}){}", c.dim, c.reset))
                        .unwrap_or_default();
                    println!(
                        "{st_col}{:<10}{} {:<12} {}  {}{}{}{det}",
                        s.status.label(),
                        c.reset,
                        platform::sanitize_display(&s.tool),
                        platform::sanitize_display(&picker::display_path(&s.path, home.as_deref())),
                        c.dim,
                        rank::time_ago(s.updated_at, now),
                        c.reset,
                    );
                }
                return Ok(());
            }
            let (entries, sessions) = session_data(&db, &ignore, now)?;
            if entries.is_empty() {
                bail!("no live sessions. Run `grace setup --all` (or `grace setup claude`), then start the agent");
            }
            let reg = tools::Registry::from_db(&db)?;
            let data = picker::Data {
                entries,
                sessions,
                editors: Vec::new(),
                branches: std::collections::HashMap::new(),
                installed: None,
            };
            let q = query.as_deref().unwrap_or("");
            travel(
                TravelOpts {
                    db: &db,
                    reg: &reg,
                    cfg: &cfg,
                    ignore: &ignore,
                    view: picker::View::Sessions,
                    tag: None,
                    query: q,
                    open,
                    with: false,
                    full,
                    initial_toast: None,
                },
                data,
            )?;
        }
        Cmd::Hub {
            action,
            path,
            no_attach,
            run_tool,
        } => match action {
            None => {
                let _ = first_run_import_if_needed(&db, &ignore, fresh);
                let p = resolve(path)?;
                if !Path::new(&p).is_dir() {
                    bail!("not a folder: {p}");
                }
                let reg = tools::Registry::from_db(&db)?;
                let resolved_run = match run_tool {
                    Some(tag) => {
                        if !tools::valid_name(&tag) {
                            bail!("invalid tool name `{tag}`");
                        }
                        let canonical = reg.normalize_tag(&tag);
                        let Some(cmd) = reg.launch_command(&canonical) else {
                            bail!("unknown tool `{tag}`");
                        };
                        if !tools::valid_launch(&cmd) || platform::has_line_break_or_nul(&cmd) {
                            bail!("invalid launch command for `{canonical}`");
                        }
                        if !ignore.is_ignored(&p) {
                            let _ = db.record_tool(&p, &canonical, now);
                        }
                        Some((canonical, cmd))
                    }
                    None => None,
                };
                let run = resolved_run
                    .as_ref()
                    .map(|(tag, cmd)| hub::HubRun::tool(tag, cmd));
                hub::start(&db, &p, !no_attach, run)?
            }
            Some(HubCmd::Toggle { what, window }) => hub::toggle(&db, &what, window.as_deref())?,
            Some(HubCmd::Sidebar { path }) => {
                let reg = tools::Registry::from_db(&db)?;
                hub::run_sidebar(&db, &reg, &ignore, &resolve(Some(path))?)?
            }
            Some(HubCmd::Shell { .. })
            | Some(HubCmd::Keys { .. })
            | Some(HubCmd::Wake)
            | Some(HubCmd::Quit) => {}
        },
        Cmd::Add { path } => {
            let p = resolve(Some(path))?;
            if !ignore.is_ignored(&p) {
                db.record_visit(&p, now)?;
            }
        }
        Cmd::Tag { path, tool, target } => {
            let cwd = resolve(Some(path))?;
            let p = platform::tag_target(&cwd, target.as_deref());
            if !ignore.is_ignored(&p) {
                db.record_tool(&p, &db.normalize_tool_tag(&tool), now)?;
            }
        }
        Cmd::List { tag, json } => {
            let reg = tools::Registry::from_db(&db)?;
            let _ = first_run_import_if_needed(&db, &ignore, fresh);
            let entries = load(&db, &reg, &ignore, tag.as_deref())?;
            if json {
                let items: Vec<_> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "path": e.path,
                            "visits": e.visits,
                            "last_visit": e.last_visit,
                            "pinned": e.pinned,
                            "tags": e.tags.iter().map(|t| serde_json::json!({
                                "tool": t.tool,
                                "count": t.count,
                                "last_used": t.last_used,
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
                return Ok(());
            }
            let c = theme::Ansi::stdout(&cfg);
            for e in entries {
                let tags: Vec<_> = e
                    .tags
                    .iter()
                    .map(|t| {
                        format!(
                            "{}[{}]{}",
                            c.cyan,
                            platform::sanitize_display(&t.tool),
                            c.reset
                        )
                    })
                    .collect();
                let pin = if e.pinned {
                    format!("{}*{} ", c.gold, c.reset)
                } else {
                    "  ".into()
                };
                let shown = platform::sanitize_display(&e.path);
                println!(
                    "{pin}{:<60} {}{:>8}{}  {}",
                    shown,
                    c.dim,
                    rank::time_ago(e.last_visit, now),
                    c.reset,
                    tags.join(" ")
                );
            }
        }
        Cmd::Pin { path } => {
            let p = resolve(path)?;
            if !Path::new(&p).is_dir() {
                bail!("not a folder: {p}");
            }
            if ignore.is_ignored(&p) {
                bail!("folder matches an ignore rule: {p}");
            }
            db.set_pinned(&p, true)?;
            if std::io::stderr().is_terminal() {
                let a = theme::Ansi::stderr(&cfg);
                eprintln!("{} pinned {}", a.star, platform::sanitize_display(&p));
            }
        }
        Cmd::Unpin { path } => {
            let p = resolve(path)?;
            if db.set_pinned(&p, false)? {
                if std::io::stderr().is_terminal() {
                    eprintln!("unpinned {}", platform::sanitize_display(&p));
                }
            } else if std::io::stderr().is_terminal() {
                eprintln!("not tracked: {}", platform::sanitize_display(&p));
            }
        }
        Cmd::Forget { path } => {
            let p = resolve(path)?;
            if db.forget(&p)? {
                if std::io::stderr().is_terminal() {
                    eprintln!("forgot {}", platform::sanitize_display(&p));
                }
            } else if std::io::stderr().is_terminal() {
                eprintln!("not tracked: {}", platform::sanitize_display(&p));
            }
        }
        Cmd::Prune {
            dry_run,
            older_than,
        } => {
            let missing = db.list_missing(older_than, now)?;
            if missing.is_empty() {
                println!("No missing folders to prune.");
                return Ok(());
            }
            let c = theme::Ansi::stdout(&cfg);
            for e in &missing {
                println!(
                    "  {}{}{} {}",
                    c.yellow,
                    if dry_run { "would remove" } else { "removing" },
                    c.reset,
                    platform::sanitize_display(&e.path)
                );
            }
            if dry_run {
                println!(
                    "{} missing folder(s) would be pruned (dry run).",
                    missing.len()
                );
            } else {
                let paths: Vec<String> = missing.into_iter().map(|e| e.path).collect();
                let removed = db.delete_paths(&paths)?;
                println!("{}Pruned {removed} missing folder(s).{}", c.green, c.reset);
            }
        }
        Cmd::Stats => {
            let _ = first_run_import_if_needed(&db, &ignore, fresh);
            let s = db.stats(now)?;
            print_stats(&cfg, &s, home.as_deref());
        }
        Cmd::Import { source } => {
            let c = theme::Ansi::stdout(&cfg);
            if source == "all" {
                let mut total = 0usize;
                for (name, n) in import_editors::import_all(&db, &ignore) {
                    total += n;
                    println!("  {}{:<12}{} {:>4} folders", c.cyan, name, c.reset, n);
                }
                editors_live::mark_imported_now(&db);
                println!("  {}total: {total} imported entries{}", c.dim, c.reset);
            } else {
                match import_editors::import_source(&db, &source, &ignore) {
                    Some(n) => println!("  {}{source}{}: {n} folders", c.cyan, c.reset),
                    None => println!("  {}{source}{}: not found", c.dim, c.reset),
                }
                editors_live::mark_imported_now(&db);
            }
        }
        Cmd::Tool { action } => match action {
            ToolCmd::Add {
                command,
                tag,
                launch,
            } => {
                let tag = tag.unwrap_or_else(|| command.to_lowercase()).to_lowercase();
                let launch = launch.unwrap_or_else(|| command.clone());
                tools::validate(&command, &tag, &launch)?;
                db.add_tool(&command, &tag, &launch)?;
                eprintln!(
                    "{} added {command} → [{tag}]. {NEW_TERMINAL}",
                    theme::Ansi::stderr(&cfg).check
                );
            }
            ToolCmd::Rm { command } => {
                if db.remove_tool(&command)? {
                    eprintln!("removed {command}. {NEW_TERMINAL}");
                } else if tools::TOOLS
                    .iter()
                    .any(|(c, ..)| c.eq_ignore_ascii_case(&command))
                {
                    bail!("{command} is a built-in tool and cannot be removed");
                } else {
                    bail!("no custom tool named {command}");
                }
            }
            ToolCmd::List => {
                let c = theme::Ansi::stdout(&cfg);
                for t in tools::Registry::from_db(&db)?.tools() {
                    println!(
                        "{}{:<16}{} {:<16} {:<24} {}{}{}",
                        c.cyan,
                        t.command,
                        c.reset,
                        t.tag,
                        t.launch,
                        c.dim,
                        t.kind_label(),
                        c.reset
                    );
                }
            }
        },
        Cmd::Setup { .. }
        | Cmd::Session { .. }
        | Cmd::Doctor { .. }
        | Cmd::Diff { .. }
        | Cmd::Tree { .. }
        | Cmd::Init { .. }
        | Cmd::Completions { .. } => {}
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let clicolor_off = std::env::var("CLICOLOR").is_ok_and(|v| v == "0");
        let dumb = std::env::var("TERM").is_ok_and(|v| v == "dumb");
        let tty = std::io::stderr().is_terminal() && !no_color && !clicolor_off && !dumb;
        if tty {
            eprintln!("\x1b[1;31m✗ error:\x1b[0m {e:#}");
        } else {
            eprintln!("error: {e:#}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_new_and_top_level_forms() {
        let c = parse_cli_from(["grace", "knn"]).unwrap();
        assert!(c.cmd.is_none());
        assert_eq!(c.query.as_deref(), Some("knn"));

        let c = parse_cli_from(["grace", "-t", "claude", "-o", "knn"]).unwrap();
        assert!(c.cmd.is_none());
        assert_eq!(c.tag.as_deref(), Some("claude"));
        assert!(c.open);
        assert_eq!(c.query.as_deref(), Some("knn"));

        let c = parse_cli_from(["grace", "--full", "list", "--json"]).unwrap();
        assert!(c.full);
        assert!(matches!(c.cmd, Some(Cmd::List { json: true, .. })));

        let c = parse_cli_from(["grace", "pick", "--", "--full"]).unwrap();
        assert!(!c.full);
        assert!(matches!(c.cmd, Some(Cmd::Pick { query: Some(ref q), .. }) if q == "--full"));

        let c = parse_cli_from(["grace", "setup", "--all"]).unwrap();
        assert!(matches!(
            c.cmd,
            Some(Cmd::Setup {
                target: None,
                all: true,
                remove: false
            })
        ));

        let c = parse_cli_from(["grace", "doctor", "--fix"]).unwrap();
        assert!(matches!(c.cmd, Some(Cmd::Doctor { fix: true })));

        let c = parse_cli_from(["grace", "prune", "--dry-run", "--older-than", "30"]).unwrap();
        assert!(matches!(
            c.cmd,
            Some(Cmd::Prune {
                dry_run: true,
                older_than: Some(30)
            })
        ));

        let c = parse_cli_from(["grace", "hub", "shell", "--run", "claude"]).unwrap();
        assert!(matches!(
            c.cmd,
            Some(Cmd::Hub {
                action: Some(HubCmd::Shell { .. }),
                ..
            })
        ));

        assert!(parse_cli_from(["grace", "setup", "--all", "claude"]).is_err());

        let c = parse_cli_from([
            "grace", "session", "start", "--tag", "--pid", "42", "--target", "sub", "--", "/proj",
            "claude",
        ])
        .unwrap();
        assert!(matches!(
            c.cmd,
            Some(Cmd::Session {
                action: SessionCmd::Start {
                    pid: Some(42),
                    tag: true,
                    target: Some(ref tgt),
                    ref tool,
                    ..
                }
            }) if tgt == "sub" && tool == "claude"
        ));
    }

    #[test]
    fn emit_editor_cmd_preserves_args_and_quotes() {
        let ed = picker::EditorChoice {
            prog: "nvim".into(),
            args: vec!["-u".into(), "NONE".into(), "/tmp/my project".into()],
            in_terminal: true,
        };
        let cmd = emit_editor_cmd(&ed).unwrap();
        assert!(cmd.contains("nvim"), "{cmd}");
        assert!(cmd.contains("-u"), "{cmd}");
        assert!(cmd.contains("NONE"), "{cmd}");
        assert!(cmd.contains("'/tmp/my project'"), "{cmd}");

        let bad = picker::EditorChoice {
            prog: "nvim".into(),
            args: vec!["bad\narg".into()],
            in_terminal: true,
        };
        assert!(emit_editor_cmd(&bad).is_err());
    }

    #[test]
    fn emit_line_rejects_newlines_and_nul() {
        assert!(validate_emit_line("/tmp/clean/path", "path").is_ok());
        assert!(validate_emit_line("/tmp/bad\npath", "path").is_err());
        assert!(validate_emit_line("/tmp/bad\rpath", "path").is_err());
        assert!(validate_emit_line("/tmp/bad\0path", "path").is_err());
    }

    #[test]
    fn cli_parses_launcher_and_run_tool_flags() {
        let c = parse_cli_from(["grace", "-w"]).unwrap();
        assert!(c.with);
        assert!(c.cmd.is_none());

        let c = parse_cli_from(["grace", "--with", "knn"]).unwrap();
        assert!(c.with);
        assert_eq!(c.query.as_deref(), Some("knn"));

        let c = parse_cli_from(["grace", "pick", "-w", "knn"]).unwrap();
        assert!(matches!(
            c.cmd,
            Some(Cmd::Pick {
                with: true,
                query: Some(ref q),
                ..
            }) if q == "knn"
        ));

        let c = parse_cli_from(["grace", "hub", "--run-tool", "opencode"]).unwrap();
        assert!(matches!(
            c.cmd,
            Some(Cmd::Hub {
                action: None,
                run_tool: Some(ref t),
                ..
            }) if t == "opencode"
        ));

        let c = parse_cli_from(["grace", "hub", "shell", "--run-tool", "claude"]).unwrap();
        assert!(matches!(
            c.cmd,
            Some(Cmd::Hub {
                action: Some(HubCmd::Shell {
                    run_tool: Some(ref t),
                    ..
                }),
                ..
            }) if t == "claude"
        ));
    }

    #[test]
    fn format_launch_lines_handles_hub_and_shell_modes() {
        let reg = tools::Registry::new(&[]);
        let entry = model::Entry {
            path: "/tmp/myproj".into(),
            visits: 1,
            last_visit: 0,
            pinned: false,
            tags: Vec::new(),
        };
        let cli_target = launcher::InstalledTarget {
            id: "cli:opencode".into(),
            label: "opencode".into(),
            tag: "opencode".into(),
            kind: launcher::TargetKind::CliAgent,
            method: launcher::LaunchMethod::CliAgent {
                tool_tag: "opencode".into(),
                launch_cmd: "opencode".into(),
            },
        };

        let hub_lines = format_launch_lines(
            &reg,
            true,
            &entry,
            &launcher::LaunchChoice {
                target: cli_target.clone(),
                force_shell: false,
            },
        )
        .unwrap();
        assert_eq!(
            hub_lines,
            (
                "/tmp/myproj".into(),
                Some("grace hub --run-tool opencode".into())
            )
        );

        let shell_lines = format_launch_lines(
            &reg,
            true,
            &entry,
            &launcher::LaunchChoice {
                target: cli_target.clone(),
                force_shell: true,
            },
        )
        .unwrap();
        assert_eq!(shell_lines, ("/tmp/myproj".into(), Some("opencode".into())));

        let no_hub_lines = format_launch_lines(
            &reg,
            false,
            &entry,
            &launcher::LaunchChoice {
                target: cli_target,
                force_shell: false,
            },
        )
        .unwrap();
        assert_eq!(
            no_hub_lines,
            ("/tmp/myproj".into(), Some("opencode".into()))
        );
    }
}
