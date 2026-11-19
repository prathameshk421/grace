mod db;
mod import;
mod import_editors;
mod model;
mod picker;
mod platform;
mod rank;
mod shell;
mod tools;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "grace", version, about = "Fast travel between your project folders")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the picker. Prints the chosen path (the shell function then runs cd).
    Pick {
        /// Show only folders with this tool tag, for example "claude".
        #[arg(short, long)]
        tag: Option<String>,
        /// Also print the launch command of the last tool used there, on a second line.
        #[arg(short, long)]
        open: bool,
        /// Start the picker with this search text.
        query: Option<String>,
    },
    /// Record a visit to a folder (the shell hook calls this).
    Add { path: PathBuf },
    /// Record that an AI tool ran in a folder (the shell hook calls this).
    Tag {
        /// The folder where the command ran.
        path: PathBuf,
        tool: String,
        /// Folder argument of the command. Relative to <path>. Used only if it is a folder.
        target: Option<String>,
    },
    /// Print tracked folders, best first.
    List {
        #[arg(short, long)]
        tag: Option<String>,
    },
    /// Keep a folder at the top of the list. Default: the current folder.
    Pin { path: Option<PathBuf> },
    /// Remove the pin from a folder. Default: the current folder.
    Unpin { path: Option<PathBuf> },
    /// Remove a folder from grace. Default: the current folder.
    Forget { path: Option<PathBuf> },
    /// Import folders from Claude Code history and editor history (read-only).
    Import {
        /// Where to import from. Default: all.
        #[arg(default_value = "all", value_parser = clap::builder::PossibleValuesParser::new(import_editors::SOURCES))]
        source: String,
    },
    /// Add, remove or list AI tools.
    Tool {
        #[command(subcommand)]
        action: ToolCmd,
    },
    /// Print the shell setup. Supported shells: zsh, bash, fish, powershell (or pwsh).
    /// zsh: add `eval "$(grace init zsh)"` to ~/.zshrc. bash: same line in ~/.bashrc.
    /// fish: add `grace init fish | source` to config.fish.
    /// PowerShell: add `grace init powershell | Out-String | Invoke-Expression` to $PROFILE.
    Init { shell: String },
}

#[derive(Subcommand)]
enum ToolCmd {
    /// Register a command as an AI tool, for example `grace tool add kiro`.
    Add {
        /// The command name you type in the shell.
        command: String,
        /// Tag to show for this tool. Default: the command in lowercase.
        #[arg(long)]
        tag: Option<String>,
        /// Command that starts the tool. Default: the command.
        #[arg(long)]
        launch: Option<String>,
    },
    /// Remove a custom tool. Built-in tools cannot be removed.
    Rm { command: String },
    /// Print all tools: command, tag, launch command, and kind (ai, editor or custom).
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

fn main() -> Result<()> {
    let cli = Cli::parse();
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
            if let Some(e) = picker::pick(entries, query.as_deref().unwrap_or(""), now)? {
                println!("{}", e.path);
                if open {
                    if let Some(cmd) = e.last_tool().and_then(|t| reg.launch_command(t)) {
                        println!("{cmd}");
                    }
                }
            } else {
                std::process::exit(1);
            }
        }
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
            Some(sh) => print!("{}", shell::init(sh, &reg.command_names())),
            None => bail!("unsupported shell: {shell} (use zsh, bash, fish, powershell or pwsh)"),
        },
    }
    Ok(())
}
