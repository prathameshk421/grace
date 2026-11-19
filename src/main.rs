mod db;
mod import;
mod import_editors;
mod model;
mod picker;
mod platform;
mod rank;
mod session;
mod setup;
mod shell;
mod tools;

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
    /// Print the shell setup code
    #[command(after_long_help = INIT_HELP, after_help = INIT_HELP)]
    Init {
        /// zsh, bash, fish, powershell (or pwsh)
        shell: String,
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

const NEW_TERMINAL: &str = "Open a new terminal (or run: exec zsh) to start tracking it.";

fn resolve(path: Option<PathBuf>) -> Result<String> {
    let p = match path {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    Ok(platform::normalize(&p))
}

fn load(db: &db::Db, reg: &tools::Registry, tag: Option<&str>) -> Result<Vec<model::Entry>> {
    let now = model::now();
    let mut entries: Vec<_> = db
        .list_entries()?
        .into_iter()
        .filter(|e| std::path::Path::new(&e.path).is_dir())
        .filter(|e| tag.map_or(true, |t| e.has_tag(&reg.normalize_tag(t))))
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

/// Run the picker and print the chosen path (and the launch command with --open).
fn travel(
    reg: &tools::Registry,
    entries: Vec<model::Entry>,
    sessions: &[model::Session],
    query: &str,
    open: bool,
    now: i64,
) -> Result<()> {
    if let Some(e) = picker::pick(entries, sessions, query, now)? {
        println!("{}", e.path);
        if open {
            if let Some(cmd) = e.last_tool().and_then(|t| reg.launch_command(t)) {
                println!("{cmd}");
            }
        }
    } else {
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Cmd::Session { action }) = cli.cmd {
        return run_session(action);
    }
    let (db, fresh) = db::Db::open_default()?;
    if fresh {
        // First run: fill the list from Claude Code and editor history.
        let _ = import_editors::import_all(&db);
    }
    let now = model::now();
    let reg = tools::Registry::from_db(&db)?;

    match cli.cmd.unwrap_or(Cmd::Pick { tag: None, open: false, query: None }) {
        Cmd::Pick { tag, open, query } => {
            let entries = load(&db, &reg, tag.as_deref())?;
            if entries.is_empty() {
                bail!("no folders yet. cd around a bit, or run `grace import`");
            }
            let sessions = db.sessions(now).unwrap_or_default();
            travel(&reg, entries, &sessions, query.as_deref().unwrap_or(""), open, now)?;
        }
        Cmd::Sessions { list, open, query } => {
            let mut sessions = db.sessions(now)?;
            session::sort_sessions(&mut sessions);
            if list {
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
            let best = picker::best_by_path(&sessions);
            let mut entries: Vec<_> = load(&db, &reg, None)?
                .into_iter()
                .filter(|e| best.contains_key(&e.path))
                .collect();
            if entries.is_empty() {
                bail!("no live sessions. Run `grace setup claude` or `grace setup codex`, then start the agent");
            }
            let key = |e: &model::Entry| {
                let b = &best[&e.path];
                (picker::urgency(b.status), b.updated_at)
            };
            entries.sort_by(|a, b| key(b).cmp(&key(a)));
            travel(&reg, entries, &sessions, query.as_deref().unwrap_or(""), open, now)?;
        }
        Cmd::Setup { target, remove } => setup::run(&target, remove)?,
        Cmd::Session { .. } => {}
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
