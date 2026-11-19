mod db;
mod diffview;
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
mod tools;
mod treeview;
mod tui;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

const AFTER_HELP: &str = "\
Examples:
  grace                 open the picker
  grace knn             jump to the only match for \"knn\"
  grace -t claude       only folders where you used Claude
  grace -o              travel and start the last tool used there
  grace pin             keep this folder at the top
  grace list            print tracked folders, best first
  grace import          import folders from Claude Code and editors
  grace tool add kiro   track a new AI tool
  grace init zsh        print the shell setup code
  grace sessions        see which agent works or waits in which folder
  grace setup claude    let grace see the status of Claude Code
  grace hub             open the hub: sidebar, shell, diff and tree (needs tmux)
  grace hub ~/code/knn  open the hub on that folder
  grace diff            see the git changes of this folder
  grace tree            browse the files of this folder

Setup: run `grace init --help` to see the line for your shell.";

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

Open a new terminal after you save the file.";

#[derive(Parser)]
#[command(
    name = "grace",
    version,
    about = "Fast travel between your project folders",
    long_about = "Fast travel between your project folders.\nRun `grace` with no command to open the picker.",
    after_help = AFTER_HELP
)]
struct Cli {
    /// Draw the picker on the full screen, not below the prompt (or set GRACE_FULLSCREEN=1)
    #[arg(long, global = true)]
    full: bool,
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
        /// Also print the launch command of the last tool used there
        #[arg(short, long)]
        open: bool,
        /// Start the picker with this search text
        query: Option<String>,
    },
    /// Connect the Claude Code CLI or the Codex CLI so grace sees their status
    Setup {
        /// claude or codex
        target: String,
        /// Undo the setup
        #[arg(long)]
        remove: bool,
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
    Shell,
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
  r  right side   z  zoom main    g  go to sidebar
  q  detach       Q  quit hub     ?  all keys

Leave the hub:
  ctrl-g q         detach. The hub and your agents keep running.
                   Run `grace hub` to go back.
  ctrl-g Q         quit. Closes every workspace and stops the hub.
  grace hub quit   the same, from any terminal.
  exit             in a workspace shell, closes that workspace.

In the sidebar, type to filter. Enter opens the folder in its own workspace.
ctrl-o also starts the AI tool you used there last.";

const NEW_TERMINAL: &str = "Open a new terminal (or run: exec zsh) to start tracking it.";

fn resolve(path: Option<PathBuf>) -> Result<String> {
    let p = match path {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    Ok(platform::normalize(&p))
}

pub(crate) fn load(db: &db::Db, reg: &tools::Registry, tag: Option<&str>) -> Result<Vec<model::Entry>> {
    let now = model::now();
    let mut entries: Vec<_> = db
        .list_entries()?
        .into_iter()
        .filter(|e| std::path::Path::new(&e.path).is_dir())
        .filter(|e| tag.is_none_or(|t| e.has_tag(&reg.normalize_tag(t))))
        .collect();
    rank::sort_entries(&mut entries, now);
    Ok(entries)
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
    match action {
        SessionCmd::Start { path, tool, pid } => {
            let tool = reg.normalize_tag(&tool);
            let p = resolve(Some(path))?;
            if reg.is_ai_tag(&tool) && !platform::ignored(&p) {
                db.session_start(&p, &tool, pid, now)?;
            }
        }
        SessionCmd::End { pid } => db.session_end_pid(pid, now)?,
        SessionCmd::Set { path, tool, status } => {
            let Some(status) = model::Status::parse(&status) else {
                bail!("status must be working, waiting, ready or done");
            };
            let tool = reg.normalize_tag(&tool);
            let p = resolve(Some(path))?;
            if reg.is_ai_tag(&tool) && !platform::ignored(&p) {
                db.session_set(&p, &tool, status, now)?;
            }
        }
        SessionCmd::Hook { .. } => {}
    }
    Ok(())
}

fn run_hook(agent: HookCmd) -> Result<()> {
    use std::io::Read;
    let (tool, status, cwd) = match agent {
        HookCmd::Claude => {
            let mut input = String::new();
            std::io::stdin().take(1 << 20).read_to_string(&mut input)?;
            let Some((status, cwd)) = session::parse_claude_hook(&input) else {
                return Ok(());
            };
            ("claude", status, cwd)
        }
        HookCmd::Codex { args } => {
            let (json, chained) = session::split_codex_args(&args);
            // Own update first, then forward. Forward even when our update fails.
            let own = json
                .and_then(session::parse_codex_notify)
                .map(|cwd| set_hook_status("codex", model::Status::Ready, cwd));
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
            if integ.reads_stdin {
                use std::io::IsTerminal;
                if !std::io::stdin().is_terminal() {
                    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
                }
            }
            let res = (integ.hook)(&argv[1..], &input);
            // The CLI may parse our stdout: print what it expects, even if our update fails.
            if !integ.stdout.is_empty() {
                println!("{}", integ.stdout);
            }
            let Some(status) = res.status else {
                return Ok(());
            };
            (integ.tool, status, res.cwd)
        }
    };
    set_hook_status(tool, status, cwd)
}

/// Run the chained notify program with the same JSON, detached. Errors are ignored.
fn forward_codex(chained: &[String], json: &str) {
    use std::process::{Command, Stdio};
    let _ = Command::new(&chained[0])
        .args(&chained[1..])
        .arg(json)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn set_hook_status(tool: &str, status: model::Status, cwd: Option<String>) -> Result<()> {
    let p = resolve(cwd.map(PathBuf::from))?;
    if platform::ignored(&p) {
        return Ok(());
    }
    let (db, _) = db::Db::open_default()?;
    let tool = tools::Registry::from_db(&db)?.normalize_tag(tool);
    db.session_set(&p, &tool, status, model::now())
}

/// Quote a path for the shell that runs line 2 of the output.
fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        format!("'{}'", s.replace('\'', "''"))
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The ctrl-e command: focus the open editor window, or start the editor used last there.
fn editor_command(
    reg: &tools::Registry,
    e: &model::Entry,
    open: Option<&editors_live::OpenEditor>,
) -> Option<String> {
    if let Some(o) = open {
        return Some(o.open_cmd.clone());
    }
    // Tags are sorted newest first.
    let tag = e.tags.iter().find(|t| !reg.is_ai_tag(&t.tool))?;
    let launch = reg.launch_command(&tag.tool)?;
    let base = launch.strip_suffix(" .").unwrap_or(&launch);
    Some(format!("{base} {}", shell_quote(&e.path)))
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
    let mut entries: Vec<_> =
        load(db, reg, None)?.into_iter().filter(|e| best.contains_key(&e.path)).collect();
    let key = |e: &model::Entry| {
        let b = &best[&e.path];
        (picker::urgency(b.status), b.updated_at)
    };
    entries.sort_by_key(|e| std::cmp::Reverse(key(e)));
    Ok((entries, sessions))
}

/// Run the picker and print the result for the shell function.
/// Line 1 is the folder to cd to (empty: stay). Line 2, if present, is a command to run.
#[allow(clippy::too_many_arguments)]
fn travel(
    db: &db::Db,
    reg: &tools::Registry,
    data: picker::Data,
    view: picker::View,
    tag: Option<&str>,
    query: &str,
    open: bool,
    full: bool,
) -> Result<()> {
    let reload = move || {
        let now = model::now();
        let (entries, sessions) = match view {
            picker::View::Folders => {
                (load(db, reg, tag).ok()?, db.sessions(now).unwrap_or_default())
            }
            picker::View::Sessions => session_data(db, reg, now).ok()?,
        };
        Some(picker::Data { entries, sessions, editors: editors_live::open_editors() })
    };
    let opts = picker::Opts {
        view,
        fullscreen: full,
        editor_cmd: Box::new(|e, o| editor_command(reg, e, o)),
        // Editor history import can take ~100 ms, so it runs after the first frame.
        after_first_frame: Some(Box::new(|| editors_live::refresh_if_stale(db))),
        reload: Some(Box::new(reload)),
        reload_every: match view {
            picker::View::Folders => None,
            picker::View::Sessions => Some(std::time::Duration::from_secs(1)),
        },
    };
    let Some(choice) = picker::pick(data, query, model::now(), opts)? else {
        std::process::exit(1);
    };
    let e = &choice.entry;
    match choice.action {
        picker::Action::Editor(cmd) => {
            println!();
            println!("{cmd}");
        }
        action => {
            println!("{}", e.path);
            if open || action == picker::Action::Resume {
                if let Some(cmd) = e.last_tool().and_then(|t| reg.launch_command(t)) {
                    println!("{cmd}");
                }
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Session { action }) => return run_session(action),
        Some(Cmd::Diff { path }) => {
            let p = resolve(path)?;
            let in_hub = hub::pane_in_hub("diff");
            tui::run(&mut diffview::DiffView::new(p.into(), picker::Theme::from_env()).in_hub(in_hub))?;
            return hub::pane_closed(in_hub, "diff");
        }
        Some(Cmd::Tree { path }) => {
            let p = resolve(path)?;
            let in_hub = hub::pane_in_hub("tree");
            tui::run(&mut treeview::TreeView::new(p.into(), picker::Theme::from_env()).in_hub(in_hub))?;
            return hub::pane_closed(in_hub, "tree");
        }
        Some(Cmd::Hub { action: Some(HubCmd::Shell), .. }) => return hub::shell(),
        Some(Cmd::Hub { action: Some(HubCmd::Quit), .. }) => return hub::quit(),
        Some(Cmd::Hub { action: Some(HubCmd::Keys { wait }), .. }) => return hub::keys(wait),
        _ => {}
    }
    let (db, fresh) = db::Db::open_default()?;
    if fresh {
        // First run: fill the list from Claude Code and editor history.
        let _ = import_editors::import_all(&db);
    }
    let now = model::now();
    let reg = tools::Registry::from_db(&db)?;
    let full = cli.full || std::env::var("GRACE_FULLSCREEN").is_ok_and(|v| v == "1");

    match cli.cmd.unwrap_or(Cmd::Pick { tag: None, open: false, query: None }) {
        Cmd::Pick { tag, open, query } => {
            // open_editors is fast (< 30 ms), so it may run before the first frame.
            let data = picker::Data {
                entries: load(&db, &reg, tag.as_deref())?,
                sessions: db.sessions(now).unwrap_or_default(),
                editors: editors_live::open_editors(),
            };
            let q = query.as_deref().unwrap_or("");
            travel(&db, &reg, data, picker::View::Folders, tag.as_deref(), q, open, full)?;
        }
        Cmd::Sessions { list, open, query } => {
            if list {
                let mut sessions = db.sessions(now)?;
                session::sort_sessions(&mut sessions);
                let home = platform::home_dir().map(|h| h.to_string_lossy().into_owned());
                for s in &sessions {
                    println!(
                        "{:<9} {:<12} {}  {}",
                        s.status.label(),
                        s.tool,
                        picker::display_path(&s.path, home.as_deref()),
                        rank::time_ago(s.updated_at, now)
                    );
                }
                return Ok(());
            }
            let (entries, sessions) = session_data(&db, &reg, now)?;
            if entries.is_empty() {
                bail!("no live sessions. Run `grace setup claude` or `grace setup codex`, then start the agent");
            }
            let data = picker::Data { entries, sessions, editors: editors_live::open_editors() };
            let q = query.as_deref().unwrap_or("");
            travel(&db, &reg, data, picker::View::Sessions, None, q, open, full)?;
        }
        Cmd::Setup { target, remove } => setup::run(&target, remove)?,
        Cmd::Session { .. } | Cmd::Diff { .. } | Cmd::Tree { .. } => {}
        Cmd::Hub { action, path, no_attach } => match action {
            None => {
                let p = resolve(path)?;
                if !std::path::Path::new(&p).is_dir() {
                    bail!("not a folder: {p}");
                }
                hub::start(&db, &p, !no_attach)?
            }
            Some(HubCmd::Toggle { what, window }) => hub::toggle(&db, &what, window.as_deref())?,
            Some(HubCmd::Sidebar { path }) => hub::run_sidebar(&db, &reg, &resolve(Some(path))?)?,
            Some(HubCmd::Shell) | Some(HubCmd::Keys { .. }) | Some(HubCmd::Quit) => {}
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
        Cmd::List { tag } => {
            for e in load(&db, &reg, tag.as_deref())? {
                let tags: Vec<_> = e.tags.iter().map(|t| format!("[{}]", t.tool)).collect();
                let pin = if e.pinned { "* " } else { "  " };
                println!(
                    "{pin}{:<60} {:>8}  {}",
                    e.path,
                    rank::time_ago(e.last_visit, now),
                    tags.join(" ")
                );
            }
        }
        Cmd::Pin { path } => db.set_pinned(&resolve(path)?, true)?,
        Cmd::Unpin { path } => db.set_pinned(&resolve(path)?, false)?,
        Cmd::Forget { path } => db.forget(&resolve(path)?)?,
        Cmd::Import { source } => {
            if source == "all" {
                for (name, n) in import_editors::import_all(&db) {
                    println!("{name}: {n} folders");
                }
            } else {
                match import_editors::import_source(&db, &source) {
                    Some(n) => println!("{source}: {n} folders"),
                    None => println!("{source}: not found"),
                }
            }
        }
        Cmd::Tool { action } => match action {
            ToolCmd::Add { command, tag, launch } => {
                let tag = tag.unwrap_or_else(|| command.to_lowercase()).to_lowercase();
                let launch = launch.unwrap_or_else(|| command.clone());
                tools::validate(&command, &tag, &launch)?;
                db.add_tool(&command, &tag, &launch)?;
                eprintln!("added {command} \u{2192} [{tag}]. {NEW_TERMINAL}");
            }
            ToolCmd::Rm { command } => {
                if db.remove_tool(&command)? {
                    eprintln!("removed {command}. Open a new terminal (or run: exec zsh) to stop tracking it.");
                } else if tools::TOOLS.iter().any(|(c, ..)| c.eq_ignore_ascii_case(&command)) {
                    bail!("{command} is a built-in tool and cannot be removed");
                } else {
                    bail!("no custom tool named {command}");
                }
            }
            ToolCmd::List => {
                for t in reg.tools() {
                    println!("{:<16} {:<16} {:<24} {}", t.command, t.tag, t.launch, t.kind_label());
                }
            }
        },
        Cmd::Init { shell } => match shell::Shell::parse(&shell) {
            Some(sh) => print!("{}", shell::init(sh, &reg.command_names(), &reg.ai_command_names())),
            None => bail!("unsupported shell: {shell} (use zsh, bash, fish, powershell or pwsh)"),
        },
    }
    Ok(())
}
