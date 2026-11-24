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
        #[arg(long)]
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
    },
    /// Print the keys of the hub
    #[command(hide = true)]
    Keys {
        /// Wait for a key before exit
        #[arg(long)]
        wait: bool,
    },
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
    tag: Option<&str>,
) -> Result<Vec<model::Entry>> {
    let now = model::now();
    let mut entries: Vec<_> = db
        .list_entries()?
        .into_iter()
        .filter(|e| Path::new(&e.path).is_dir())
        .filter(|e| tag.is_none_or(|t| e.has_tag(&reg.normalize_tag(t))))
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

fn collect_hub_paths() -> Vec<String> {
    if cfg!(windows) {
        return Vec::new();
    }
    let t = hub::Tmux::new();
    if !t.ok(&["has-session", "-t", &format!("={}", hub::SESSION)]) {
        return Vec::new();
    }
    t.windows()
        .map(|ws| ws.into_iter().map(|(_, p)| p).collect())
        .unwrap_or_default()
}

/// Session commands: no first-run import, so they stay fast.
fn run_session(action: SessionCmd) -> Result<()> {
    if let SessionCmd::Hook { agent } = action {
        // A hook must never fail loudly and never print to stdout.
        let _ = run_hook(agent);
        return Ok(());
    }
    let (db, _) = db::Db::open_default()?;
    let reg = tools::Registry::from_db(&db)?;
    let now = model::now();
    let pane = current_tmux_pane();
    match action {
        SessionCmd::Start { path, tool, pid } => {
            let tool = reg.normalize_tag(&tool);
            let p = resolve(Some(path))?;
            if reg.is_ai_tag(&tool) && !platform::ignored(&p) {
                db.session_start_ex(&p, &tool, pid, None, None, pane.as_deref(), now)?;
                hub::sync_glyphs_if_running(&db);
            }
        }
        SessionCmd::End { pid } => {
            db.session_end_pid(pid, now)?;
            hub::sync_glyphs_if_running(&db);
        }
        SessionCmd::Set { path, tool, status } => {
            let Some(status) = model::Status::parse(&status) else {
                bail!("status must be working, waiting, ready or done");
            };
            let tool = reg.normalize_tag(&tool);
            let p = resolve(Some(path))?;
            if reg.is_ai_tag(&tool) && !platform::ignored(&p) {
                db.session_set_ex(&p, &tool, status, None, None, pane.as_deref(), now)?;
                hub::sync_glyphs_if_running(&db);
            }
        }
        SessionCmd::Hook { .. } => {}
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
    let p = resolve(cwd.map(PathBuf::from))?;
    if platform::ignored(&p) {
        return Ok(());
    }
    let (db, _) = db::Db::open_default()?;
    let tool = tools::Registry::from_db(&db)?.normalize_tag(tool);
    let pane = current_tmux_pane();
    let sid = session_id.filter(|s| !s.is_empty());
    let det = detail
        .map(platform::sanitize_display)
        .filter(|s| !s.is_empty());
    db.session_set_ex(
        &p,
        &tool,
        status,
        sid,
        det.as_deref(),
        pane.as_deref(),
        model::now(),
    )?;
    hub::sync_glyphs_if_running(&db);
    Ok(())
}

/// Build an argv-safe editor choice for `ctrl-e`. Never uses shell evaluation for GUIs.
fn editor_choice(
    reg: &tools::Registry,
    cfg: &config::Config,
    e: &model::Entry,
    open: Option<&editors_live::OpenEditor>,
) -> Option<picker::EditorChoice> {
    if let Some(o) = open {
        return Some(picker::EditorChoice {
            prog: o.prog.clone(),
            args: o.args.clone(),
            in_terminal: false,
        });
    }
    if let Some(tag) = e.tags.iter().find(|t| reg.is_editor_tag(&t.tool)) {
        if let Some((prog, in_terminal)) = reg.editor_launch(&tag.tool) {
            return Some(picker::EditorChoice {
                prog,
                args: vec![e.path.clone()],
                in_terminal,
            });
        }
    }
    if let Some(ref fb) = cfg.editor {
        let mut parts = fb.split_whitespace();
        if let Some(prog) = parts.next() {
            if tools::valid_launch(prog) {
                let mut args: Vec<String> = parts.map(String::from).collect();
                args.push(e.path.clone());
                let base = Path::new(prog)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(prog);
                let in_terminal = matches!(base, "nvim" | "vim" | "vi" | "nano" | "hx" | "helix");
                return Some(picker::EditorChoice {
                    prog: prog.to_string(),
                    args,
                    in_terminal,
                });
            }
        }
    }
    None
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
    if let Some(tag) = e.tags.iter().find(|t| reg.is_editor_tag(&t.tool)) {
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
    reg: &tools::Registry,
    now: i64,
) -> Result<(Vec<model::Entry>, Vec<model::Session>)> {
    let mut sessions = db.sessions(now)?;
    session::sort_sessions(&mut sessions);
    let best = picker::best_by_path(&sessions);
    let mut entries: Vec<_> = load(db, reg, None)?
        .into_iter()
        .filter(|e| best.contains_key(&e.path))
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

fn spawn_gui_editor(ed: &picker::EditorChoice) -> Result<()> {
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

/// Run the picker and print the result for the shell function.
/// Line 1 is the folder to cd to (empty: stay). Line 2, if present, is a static command to run.
#[allow(clippy::too_many_arguments)]
fn travel(
    db: &db::Db,
    reg: &tools::Registry,
    cfg: &config::Config,
    data: picker::Data,
    view: picker::View,
    tag: Option<&str>,
    query: &str,
    open: bool,
    full: bool,
    initial_toast: Option<(String, theme::ToastKind)>,
) -> Result<()> {
    let editors_snap = data.editors.clone();
    let entries_snap = data.entries.clone();
    let tag_owned = tag.map(String::from);
    let reload_tag = tag_owned.clone();
    let reload = move |cur_view: picker::View| {
        let now = model::now();
        let (entries, sessions) = match cur_view {
            picker::View::Folders => (
                load(db, reg, reload_tag.as_deref()).ok()?,
                db.sessions(now).unwrap_or_default(),
            ),
            picker::View::Sessions => session_data(db, reg, now).ok()?,
        };
        let branches = collect_branches(&entries);
        let hub_paths = collect_hub_paths();
        Some(picker::Data {
            entries,
            sessions,
            editors: editors_live::open_editors(),
            branches,
            hub_paths,
        })
    };
    let bg_tag = tag_owned;
    let after_first_frame: Option<Box<dyn FnOnce() -> Option<picker::Data> + Send + 'static>> =
        Some(Box::new(move || {
            let branches = collect_branches(&entries_snap);
            let hub_paths = collect_hub_paths();
            let (bg_db, _) = db::Db::open_default().ok()?;
            let bg_reg = tools::Registry::from_db(&bg_db).ok()?;
            editors_live::refresh_if_stale_with(&bg_db, Some(&editors_snap));
            let now = model::now();
            let (entries, sessions) = match view {
                picker::View::Folders => (
                    load(&bg_db, &bg_reg, bg_tag.as_deref()).ok()?,
                    bg_db.sessions(now).unwrap_or_default(),
                ),
                picker::View::Sessions => session_data(&bg_db, &bg_reg, now).ok()?,
            };
            Some(picker::Data {
                entries,
                sessions,
                editors: editors_snap,
                branches,
                hub_paths,
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
        let count: usize = import_editors::import_all(db)
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
        if platform::ignored(&norm) {
            return Err("Current folder matches an ignore rule".into());
        }
        db.record_visit(&norm, model::now())
            .map_err(|e| e.to_string())?;
        Ok(format!("Added {}", platform::sanitize_display(&norm)))
    })
        as Box<dyn FnMut() -> std::result::Result<String, String>>);

    let opts = picker::Opts {
        view,
        fullscreen: full,
        tag_filter: tag.map(String::from),
        initial_toast,
        shell_hook_missing: detect_missing_shell_hook(),
        editor_cmd: Box::new(|e, o| editor_choice(reg, cfg, e, o)),
        tool_label: Box::new(|e| resume_tag(reg, e).map(String::from)),
        editor_label: Box::new(|e, o| editor_label(reg, cfg, e, o)),
        toggle_pin,
        forget_dir,
        import_all_now,
        add_cwd_now,
        after_first_frame,
        reload: Some(Box::new(reload)),
        reload_every: match view {
            picker::View::Folders => None,
            picker::View::Sessions => Some(std::time::Duration::from_secs(2)),
        },
    };
    let Some(choice) = picker::pick(data, query, model::now(), opts)? else {
        std::process::exit(1);
    };
    emit_choice(reg, &choice, open)
}

fn emit_choice(reg: &tools::Registry, choice: &picker::Choice, open: bool) -> Result<()> {
    let e = &choice.entry;
    match &choice.action {
        picker::Action::Editor(ed) => {
            if ed.in_terminal {
                validate_emit_line(&e.path, "path")?;
                validate_emit_line(&ed.prog, "editor command")?;
                println!("{}", e.path);
                println!("{}", ed.prog);
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
    }
    Ok(())
}

struct TermColors {
    gold: &'static str,
    green: &'static str,
    cyan: &'static str,
    yellow: &'static str,
    dim: &'static str,
    bold: &'static str,
    reset: &'static str,
}

impl TermColors {
    fn stdout() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let clicolor_off = std::env::var("CLICOLOR").is_ok_and(|v| v == "0");
        let dumb = std::env::var("TERM").is_ok_and(|v| v == "dumb");
        if std::io::stdout().is_terminal() && !no_color && !clicolor_off && !dumb {
            Self {
                gold: "\x1b[1;33m",
                green: "\x1b[32m",
                cyan: "\x1b[36m",
                yellow: "\x1b[33m",
                dim: "\x1b[2m",
                bold: "\x1b[1m",
                reset: "\x1b[0m",
            }
        } else {
            Self {
                gold: "",
                green: "",
                cyan: "",
                yellow: "",
                dim: "",
                bold: "",
                reset: "",
            }
        }
    }
}

fn first_run_import_if_needed(db: &db::Db, fresh: bool) -> Option<(String, theme::ToastKind)> {
    if !fresh {
        return None;
    }
    let _ = import::import_claude(db);
    let _ = import_editors::import_editors_quiet(db);
    editors_live::mark_imported_now(db);
    let n = db.list_entries().map(|v| v.len()).unwrap_or(0);
    if n > 0 {
        Some((
            format!("✦ Welcome to grace — discovered {n} folder(s)"),
            theme::ToastKind::Info,
        ))
    } else {
        None
    }
}

fn print_stats(stats: &model::Stats, home: Option<&str>) {
    let c = TermColors::stdout();
    println!();
    println!(
        "  {}✦ grace stats{} {}— tracked workspace overview{}",
        c.gold, c.reset, c.dim, c.reset
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
                "    {}{:<14}{} {}{:<18}{} {:>5} runs {}({} folders){}",
                c.cyan, tool_shown, c.reset, c.gold, bar, c.reset, runs, c.dim, folders, c.reset
            );
        }
    }
    if !stats.top_dirs.is_empty() {
        println!();
        println!("  {}Top folders{}", c.bold, c.reset);
        for e in &stats.top_dirs {
            let pin = if e.pinned { "★ " } else { "  " };
            let shown = platform::sanitize_display(&picker::display_path(&e.path, home));
            println!(
                "    {}{}{} {:<44} {}{} visits{}",
                c.gold, pin, c.reset, shown, c.dim, e.visits, c.reset
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
    let cfg = config::load();
    match cli.cmd {
        Some(Cmd::Session { action }) => return run_session(action),
        Some(Cmd::Doctor { fix }) => return doctor::run(fix),
        Some(Cmd::Diff { path }) => {
            let p = resolve(path)?;
            let in_hub = hub::pane_in_hub(hub::Role::Diff.as_str());
            let th = theme::Theme::from_env_and_config(&cfg);
            tui::run(&mut diffview::DiffView::new(p.into(), th).in_hub(in_hub))?;
            return hub::pane_closed(in_hub, hub::Role::Diff);
        }
        Some(Cmd::Tree { path }) => {
            let p = resolve(path)?;
            let in_hub = hub::pane_in_hub(hub::Role::Tree.as_str());
            let th = theme::Theme::from_env_and_config(&cfg);
            tui::run(&mut treeview::TreeView::new(p.into(), th).in_hub(in_hub))?;
            return hub::pane_closed(in_hub, hub::Role::Tree);
        }
        Some(Cmd::Hub {
            action: Some(HubCmd::Shell { run: cmd }),
            ..
        }) => {
            return hub::shell(cmd.as_deref());
        }
        Some(Cmd::Hub {
            action: Some(HubCmd::Keys { wait }),
            ..
        }) => return hub::keys(wait),
        Some(Cmd::Hub {
            action: Some(HubCmd::Quit),
            ..
        }) => return hub::quit(),
        Some(Cmd::Init { shell }) => {
            let Some(sh) = shell::Shell::parse(&shell) else {
                bail!("unsupported shell: {shell} (use zsh, bash, fish, powershell or pwsh)");
            };
            let (db, _) = db::Db::open_default()?;
            let reg = tools::Registry::from_db(&db)?;
            print!(
                "{}",
                shell::init(sh, &reg.command_names(), &reg.ai_command_names())
            );
            return Ok(());
        }
        Some(Cmd::Completions { shell }) => {
            let Some(sh) = shell::Shell::parse(&shell) else {
                bail!("unsupported shell: {shell} (use zsh, bash, fish, powershell or pwsh)");
            };
            print!("{}", shell::completions(sh));
            return Ok(());
        }
        Some(Cmd::Setup {
            target,
            all,
            remove,
        }) => {
            return setup::run(target.as_deref(), all, remove);
        }
        _ => {}
    }

    let full = cli.full || std::env::var("GRACE_FULLSCREEN").is_ok_and(|v| v == "1");
    let (db, fresh) = db::Db::open_default()?;
    let reg = tools::Registry::from_db(&db)?;
    let now = model::now();
    let home = platform::home_dir().map(|h| platform::clean_path_str(&h.to_string_lossy()));

    let default_pick = || Cmd::Pick {
        tag: cli.tag.clone(),
        open: cli.open,
        query: cli.query.clone(),
    };

    match cli.cmd.unwrap_or_else(default_pick) {
        Cmd::Pick { tag, open, query } => {
            let toast = first_run_import_if_needed(&db, fresh);
            let entries = load(&db, &reg, tag.as_deref())?;
            let data = picker::Data {
                entries,
                sessions: db.sessions(now).unwrap_or_default(),
                editors: editors_live::open_editors(),
                branches: std::collections::HashMap::new(),
                hub_paths: Vec::new(),
            };
            let q = query.as_deref().unwrap_or("");
            travel(
                &db,
                &reg,
                &cfg,
                data,
                picker::View::Folders,
                tag.as_deref(),
                q,
                open,
                full,
                toast,
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
                let c = TermColors::stdout();
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
            let (entries, sessions) = session_data(&db, &reg, now)?;
            if entries.is_empty() {
                bail!("no live sessions. Run `grace setup --all` (or `grace setup claude`), then start the agent");
            }
            let data = picker::Data {
                entries,
                sessions,
                editors: editors_live::open_editors(),
                branches: std::collections::HashMap::new(),
                hub_paths: Vec::new(),
            };
            let q = query.as_deref().unwrap_or("");
            travel(
                &db,
                &reg,
                &cfg,
                data,
                picker::View::Sessions,
                None,
                q,
                open,
                full,
                None,
            )?;
        }
        Cmd::Hub {
            action,
            path,
            no_attach,
        } => match action {
            None => {
                let _ = first_run_import_if_needed(&db, fresh);
                let p = resolve(path)?;
                if !Path::new(&p).is_dir() {
                    bail!("not a folder: {p}");
                }
                hub::start(&db, &p, !no_attach)?
            }
            Some(HubCmd::Toggle { what, window }) => hub::toggle(&db, &what, window.as_deref())?,
            Some(HubCmd::Sidebar { path }) => hub::run_sidebar(&db, &reg, &resolve(Some(path))?)?,
            Some(HubCmd::Shell { .. }) | Some(HubCmd::Keys { .. }) | Some(HubCmd::Quit) => {}
        },
        Cmd::Add { path } => {
            let p = resolve(Some(path))?;
            if !platform::ignored(&p) {
                db.record_visit(&p, now)?;
            }
        }
        Cmd::Tag { path, tool, target } => {
            let cwd = resolve(Some(path))?;
            let p = platform::tag_target(&cwd, target.as_deref());
            if !platform::ignored(&p) {
                db.record_tool(&p, &reg.normalize_tag(&tool), now)?;
            }
        }
        Cmd::List { tag, json } => {
            let _ = first_run_import_if_needed(&db, fresh);
            let entries = load(&db, &reg, tag.as_deref())?;
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
            let c = TermColors::stdout();
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
            db.set_pinned(&p, true)?;
            if std::io::stderr().is_terminal() {
                eprintln!("★ pinned {}", platform::sanitize_display(&p));
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
            let c = TermColors::stdout();
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
            let _ = first_run_import_if_needed(&db, fresh);
            let s = db.stats(now)?;
            print_stats(&s, home.as_deref());
        }
        Cmd::Import { source } => {
            let c = TermColors::stdout();
            if source == "all" {
                let mut total = 0usize;
                for (name, n) in import_editors::import_all(&db) {
                    total += n;
                    println!("  {}{:<12}{} {:>4} folders", c.cyan, name, c.reset, n);
                }
                editors_live::mark_imported_now(&db);
                println!("  {}total: {total} imported entries{}", c.dim, c.reset);
            } else {
                match import_editors::import_source(&db, &source) {
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
                eprintln!("✓ added {command} → [{tag}]. {NEW_TERMINAL}");
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
                let c = TermColors::stdout();
                for t in reg.tools() {
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
    }

    #[test]
    fn emit_line_rejects_newlines_and_nul() {
        assert!(validate_emit_line("/tmp/clean/path", "path").is_ok());
        assert!(validate_emit_line("/tmp/bad\npath", "path").is_err());
        assert!(validate_emit_line("/tmp/bad\rpath", "path").is_err());
        assert!(validate_emit_line("/tmp/bad\0path", "path").is_err());
    }
}
