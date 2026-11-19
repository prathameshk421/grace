//! Live editor state: which folders have an editor window open right now, and a cheap
//! background refresh of editor history so folders opened from File > Open show up.
//! OWNER: editors agent. The picker agent only calls these two functions.
use crate::db::Db;
use crate::import_editors::{config_root, uri_to_path};
use crate::platform;
use crate::setup::shell_quote;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One folder with an editor window open right now.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenEditor {
    /// Absolute folder path (normalized like other grace paths).
    pub path: String,
    /// grace tag of the editor, e.g. "vscode", "antigravity", "cursor".
    pub tag: String,
    /// Command that opens or focuses this folder in that editor, e.g. `code "/a b"`.
    pub open_cmd: String,
}

/// One editor of the VS Code family.
struct App {
    /// Folder name in the app data root, also the last part of `--user-data-dir`.
    dir: &'static str,
    tag: &'static str,
    /// CLI command that opens a folder.
    cli: &'static str,
    /// macOS bundle name without `.app`.
    bundle: &'static str,
    /// Linux binary name.
    bin: &'static str,
    /// Windows executable name.
    exe: &'static str,
}

const APPS: &[App] = &[
    App { dir: "Code", tag: "vscode", cli: "code", bundle: "Visual Studio Code", bin: "code", exe: "Code.exe" },
    App {
        dir: "Code - Insiders",
        tag: "vscode",
        cli: "code-insiders",
        bundle: "Visual Studio Code - Insiders",
        bin: "code-insiders",
        exe: "Code - Insiders.exe",
    },
    App { dir: "VSCodium", tag: "vscodium", cli: "codium", bundle: "VSCodium", bin: "codium", exe: "VSCodium.exe" },
    App { dir: "Cursor", tag: "cursor", cli: "cursor", bundle: "Cursor", bin: "cursor", exe: "Cursor.exe" },
    App { dir: "Windsurf", tag: "windsurf", cli: "windsurf", bundle: "Windsurf", bin: "windsurf", exe: "Windsurf.exe" },
    App {
        dir: "Antigravity",
        tag: "antigravity",
        cli: "antigravity",
        bundle: "Antigravity",
        bin: "antigravity",
        exe: "Antigravity.exe",
    },
    App {
        dir: "Antigravity IDE",
        tag: "antigravity",
        cli: "antigravity",
        bundle: "Antigravity IDE",
        bin: "antigravity-ide",
        exe: "Antigravity.exe",
    },
];

/// Meta key with the Unix time of the last background editor import.
const IMPORTED_AT: &str = "editors_imported_at";
/// Seconds before editor history counts as stale.
const STALE_SECS: i64 = 600;

fn app(dir: &str) -> Option<&'static App> {
    APPS.iter().find(|a| a.dir == dir)
}

// ---------- pure helpers ----------

/// Folder paths of open windows in a `storage.json` text, in order, without duplicates.
/// Workspace windows give the folder of the `.code-workspace` file.
/// Empty windows, remote windows and untitled workspaces are skipped.
pub fn parse_open_folders(json: &str, windows: bool) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(state) = v.get("windowsState") else {
        return Vec::new();
    };
    let mut wins: Vec<&serde_json::Value> = Vec::new();
    if let Some(w) = state.get("lastActiveWindow") {
        wins.push(w);
    }
    if let Some(a) = state.get("openedWindows").and_then(|a| a.as_array()) {
        wins.extend(a);
    }
    let mut out: Vec<String> = Vec::new();
    for w in wins {
        let Some(p) = window_folder(w, windows) else {
            continue;
        };
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn window_folder(w: &serde_json::Value, windows: bool) -> Option<String> {
    let s = |k: &str| w.get(k).and_then(|x| x.as_str());
    if let Some(u) = s("folder").or_else(|| s("folderUri")) {
        return uri_to_path(u, windows);
    }
    let ws = w.get("workspaceIdentifier")?;
    let cfg = ws.get("configURIPath").or_else(|| ws.get("configPath"))?.as_str()?;
    let file = if cfg.starts_with("file:") {
        uri_to_path(cfg, windows)?
    } else if cfg.contains("://") {
        return None;
    } else {
        platform::clean_path_str(cfg)
    };
    let (dir, name) = file.rsplit_once(['/', '\\'])?;
    // Untitled workspaces live in the app data as `workspace.json`.
    if !name.ends_with(".code-workspace") || dir.is_empty() {
        return None;
    }
    Some(platform::clean_path_str(dir))
}

/// Last path segment of a `/` or `\` separated string.
fn base_name(s: &str) -> &str {
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// The outer bundle (`/x/Name.app`) that holds the command of a process line.
fn bundle_of(cmd: &str) -> Option<&str> {
    cmd.find(".app/").map(|i| &cmd[..i + 4])
}

/// Map process lines to running apps: `app data folder name -> bundle path (macOS)`.
/// Unix lines are `pgrep -lf` / `pgrep -af` output (`PID command args`).
/// Windows lines are `tasklist /fo csv /nh` output.
pub fn parse_running(out: &str, windows: bool) -> HashMap<&'static str, Option<String>> {
    let mut found: HashMap<&'static str, Option<String>> = HashMap::new();
    let mut udd_bundles: HashSet<String> = HashSet::new();
    let mut main: Vec<(&'static App, Option<String>)> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if windows {
            let exe = line.trim_start_matches('"').split('"').next().unwrap_or("");
            for a in APPS.iter().filter(|a| a.exe.eq_ignore_ascii_case(exe)) {
                found.entry(a.dir).or_insert(None);
            }
            continue;
        }
        let cmd = match line.split_once(' ') {
            Some((pid, rest)) if pid.chars().all(|c| c.is_ascii_digit()) => rest.trim_start(),
            _ => line,
        };
        let bundle = bundle_of(cmd).map(String::from);
        // Helper processes name the data folder exactly.
        if let Some(i) = cmd.find("--user-data-dir=") {
            let v = &cmd[i + "--user-data-dir=".len()..];
            let v = v.split(" --").next().unwrap_or(v).trim().trim_matches('"');
            if let Some(a) = app(base_name(v.trim_end_matches(['/', '\\']))) {
                if let Some(b) = &bundle {
                    udd_bundles.insert(b.clone());
                }
                let e = found.entry(a.dir).or_insert(None);
                if e.is_none() {
                    *e = bundle;
                }
            }
            continue;
        }
        let hit = match &bundle {
            Some(b) => {
                let name = base_name(b).trim_end_matches(".app");
                APPS.iter().find(|a| a.bundle == name)
            }
            None => {
                let exe = base_name(cmd.split(' ').next().unwrap_or(cmd));
                APPS.iter().find(|a| a.bin == exe)
            }
        };
        if let Some(a) = hit {
            main.push((a, bundle));
        }
    }
    // A main process counts only when no helper of its bundle named the data folder.
    for (a, bundle) in main {
        if bundle.as_ref().is_some_and(|b| udd_bundles.contains(b)) {
            continue;
        }
        let e = found.entry(a.dir).or_insert(None);
        if e.is_none() {
            *e = bundle;
        }
    }
    found
}

/// True when the last import time is missing, bad, 10 minutes old or more, or in the future.
pub fn is_stale(last: Option<&str>, now: i64) -> bool {
    match last.and_then(|s| s.trim().parse::<i64>().ok()) {
        Some(t) => now - t >= STALE_SECS || t > now,
        None => true,
    }
}

// ---------- system ----------

fn pgrep_pattern() -> String {
    let dirs: Vec<&str> = APPS.iter().map(|a| a.dir).collect();
    let mut pat = format!("user-data-dir=.*/({})( --|$)", dirs.join("|"));
    if cfg!(target_os = "macos") {
        let bundles: Vec<&str> = APPS.iter().map(|a| a.bundle).collect();
        pat.push_str(&format!("|/({})\\.app/Contents/MacOS/", bundles.join("|")));
    } else {
        let bins: Vec<&str> = APPS.iter().map(|a| a.bin).collect();
        pat.push_str(&format!("|^(.*/)?({})( |$)", bins.join("|")));
    }
    pat
}

/// Run one process listing and return its output. Empty on any error.
fn process_listing() -> String {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("tasklist");
        c.args(["/fo", "csv", "/nh"]);
        c
    } else {
        let mut c = Command::new("pgrep");
        c.arg(if cfg!(target_os = "macos") { "-lf" } else { "-af" }).arg(pgrep_pattern());
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.stdin(Stdio::null()).stderr(Stdio::null());
    match cmd.output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// True when `name` is a command on PATH.
fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: &[&str] = if cfg!(windows) { &[".cmd", ".exe", ""] } else { &[""] };
    std::env::split_paths(&path).any(|dir| exts.iter().any(|ext| dir.join(format!("{name}{ext}")).is_file()))
}

/// Command that opens `path` in `app`.
fn open_command(app: &App, bundle: Option<&str>, path: &str, cli_on_path: bool) -> String {
    let q = shell_quote(path);
    if cli_on_path || !cfg!(target_os = "macos") {
        return format!("{} {q}", app.cli);
    }
    let bundle = bundle
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/Applications/{}.app", app.bundle)));
    let cli = bundle.join("Contents/Resources/app/bin").join(app.cli);
    if cli.is_file() {
        return format!("{} {q}", shell_quote(&cli.to_string_lossy()));
    }
    let target = if bundle.is_dir() { bundle.to_string_lossy().into_owned() } else { app.bundle.to_string() };
    format!("open -a {} {q}", shell_quote(&target))
}

/// Folders with an editor window open now. Must be fast (< 30 ms) and never fail.
pub fn open_editors() -> Vec<OpenEditor> {
    std::panic::catch_unwind(open_editors_inner).unwrap_or_default()
}

fn open_editors_inner() -> Vec<OpenEditor> {
    let running = parse_running(&process_listing(), cfg!(windows));
    if running.is_empty() {
        return Vec::new();
    }
    let Some(root) = config_root() else {
        return Vec::new();
    };
    let mut out: Vec<OpenEditor> = Vec::new();
    let mut cli_cache: HashMap<&str, bool> = HashMap::new();
    // Walk APPS in order so the output is stable.
    for a in APPS {
        let Some(bundle) = running.get(a.dir) else {
            continue;
        };
        let file = root.join(a.dir).join("User/globalStorage/storage.json");
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for p in parse_open_folders(&text, cfg!(windows)) {
            let dir = Path::new(&p);
            if !dir.is_dir() {
                continue;
            }
            let path = platform::normalize(dir);
            if platform::ignored(&path) || out.iter().any(|e| e.tag == a.tag && e.path == path) {
                continue;
            }
            let cli = *cli_cache.entry(a.cli).or_insert_with(|| on_path(a.cli));
            let open_cmd = open_command(a, bundle.as_deref(), &path, cli);
            out.push(OpenEditor { path, tag: a.tag.to_string(), open_cmd });
        }
    }
    out
}

/// Re-import editor history if the last import is older than 10 minutes. Never fails loudly.
pub fn refresh_if_stale(db: &Db) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| refresh_inner(db)));
}

fn refresh_inner(db: &Db) {
    let now = crate::model::now();
    // An open editor folder always appears in the list.
    for e in open_editors() {
        let _ = db.import_dir(&e.path, now, Some(&e.tag));
    }
    let last = db.get_meta(IMPORTED_AT).ok().flatten();
    if !is_stale(last.as_deref(), now) {
        return;
    }
    // Claim the run first so parallel calls do not repeat it.
    if db.set_meta(IMPORTED_AT, &now.to_string()).is_err() {
        return;
    }
    crate::import_editors::import_editors_quiet(db);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_storage_json() {
        let j = r#"{"theme":"x","windowsState":{
            "lastActiveWindow":{"folder":"file:///a/last","backupPath":"/b/1"},
            "openedWindows":[
                {"folder":"file:///a/one%20two","backupPath":"/b/2"},
                {"folder":"file:///a/last"},
                {"workspaceIdentifier":{"id":"w","configURIPath":"file:///ws/proj/x.code-workspace"}},
                {"workspaceIdentifier":{"id":"u","configURIPath":"file:///data/Code/Workspaces/1/workspace.json"}},
                {"backupPath":"/b/empty"},
                {"folder":"vscode-remote://ssh-remote%2Bhost/home/x"}
            ]}}"#;
        assert_eq!(parse_open_folders(j, false), vec!["/a/last", "/a/one two", "/ws/proj"]);
        assert!(parse_open_folders("{}", false).is_empty());
        assert!(parse_open_folders("garbage", false).is_empty());
        let empty = r#"{"windowsState":{"lastActiveWindow":{"backupPath":"/b"},"openedWindows":[]}}"#;
        assert!(parse_open_folders(empty, false).is_empty());
    }

    #[test]
    fn maps_mac_processes() {
        let out = "\
12128 /Applications/Visual Studio Code.app/Contents/MacOS/Code
12132 /Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper --type=gpu-process --user-data-dir=/Users/me/Library/Application Support/Code --gpu-preferences=x
500 /Applications/Antigravity.app/Contents/MacOS/Electron
501 /Applications/Antigravity.app/Contents/Frameworks/X Helper.app/Contents/MacOS/X Helper --user-data-dir=/Users/me/Library/Application Support/Antigravity IDE
600 /Users/me/Applications/Cursor.app/Contents/MacOS/Cursor
700 /Applications/Other.app/Contents/MacOS/Other --user-data-dir=/x/Brave
";
        let r = parse_running(out, false);
        let mut names: Vec<&str> = r.keys().copied().collect();
        names.sort();
        assert_eq!(names, vec!["Antigravity IDE", "Code", "Cursor"]);
        assert_eq!(r["Code"].as_deref(), Some("/Applications/Visual Studio Code.app"));
        assert_eq!(r["Cursor"].as_deref(), Some("/Users/me/Applications/Cursor.app"));
        assert!(parse_running("", false).is_empty());
    }

    #[test]
    fn maps_linux_and_windows_processes() {
        let out = "10 /usr/share/code/code\n\
                   11 /usr/share/code/code --type=zygote\n\
                   12 /opt/cursor/cursor --type=renderer --user-data-dir=/home/me/.config/Cursor --x\n";
        let r = parse_running(out, false);
        let mut names: Vec<&str> = r.keys().copied().collect();
        names.sort();
        assert_eq!(names, vec!["Code", "Cursor"]);
        let win = "\"Code.exe\",\"1\",\"Console\",\"1\",\"1 K\"\n\"explorer.exe\",\"2\",\"Console\",\"1\",\"1 K\"\n";
        let r = parse_running(win, true);
        assert_eq!(r.len(), 1);
        assert!(r.contains_key("Code"));
    }

    #[test]
    fn staleness_rule() {
        assert!(is_stale(None, 1000));
        assert!(is_stale(Some("bad"), 1000));
        assert!(!is_stale(Some("1000"), 1000));
        assert!(!is_stale(Some("500"), 1099));
        assert!(is_stale(Some("400"), 1000));
        assert!(is_stale(Some("2000"), 1000));
    }

    #[test]
    fn builds_open_command() {
        let a = app("Code").unwrap();
        let want = if cfg!(windows) { "code \"/a b\"" } else { "code '/a b'" };
        assert_eq!(open_command(a, None, "/a b", true), want);
    }

    #[test]
    fn refresh_claims_the_run() {
        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("g.db")).unwrap();
        db.set_meta(IMPORTED_AT, "0").unwrap();
        refresh_if_stale(&db);
        let t: i64 = db.get_meta(IMPORTED_AT).unwrap().unwrap().parse().unwrap();
        assert!(crate::model::now() - t < 60);
    }

    /// Smoke test on the real machine: `cargo test smoke_open_editors -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn smoke_open_editors() {
        let t = std::time::Instant::now();
        let list = open_editors();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let home = platform::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        let short = |s: &str| if home.is_empty() { s.to_string() } else { s.replace(&home, "~") };
        for e in &list {
            println!("{} [{}] {}", short(&e.path), e.tag, short(&e.open_cmd));
        }
        println!("open_editors: {} entries in {ms:.1} ms", list.len());
    }
}
